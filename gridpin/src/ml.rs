//! Parser model inference (logistic regression over hashed features) — exact
//! mirror of the training script: same features, same blake2b-4 hash, same classes.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;

pub const L_STREET: u8 = 0;
pub const L_CITY: u8 = 1;
pub const L_NUM: u8 = 2;
pub const L_REP: u8 = 3;
pub const L_PC: u8 = 4;

pub struct Parser {
    dim: usize,
    classes: usize,
    intercept: Vec<f32>,
    coef: &'static [u8], // f32 values, class-major — read unaligned
}

impl Parser {
    pub fn from_section(data: &'static [u8]) -> Option<Parser> {
        if data.len() < 9 || &data[0..4] != b"GPML" {
            return None;
        }
        let dim = u32::from_le_bytes(data[4..8].try_into().ok()?) as usize;
        let classes = data[8] as usize;
        // dim == 0 would make every feature hash `% 0` panic at query time; a model
        // with no classes has nothing to predict. Either means a corrupt section.
        if dim == 0 || classes == 0 {
            return None;
        }
        let mut intercept = Vec::with_capacity(classes);
        let mut p = 9;
        for _ in 0..classes {
            intercept.push(f32::from_le_bytes(data.get(p..p + 4)?.try_into().ok()?));
            p += 4;
        }
        // every intercept must be finite — a NaN/Inf silently corrupts every class score
        if intercept.iter().any(|x| !x.is_finite()) {
            return None;
        }
        let coef = data.get(p..)?;
        if coef.len() != classes.checked_mul(dim)?.checked_mul(4)? {
            return None;
        }
        // and so must every coefficient (validate once at load, not per query)
        if coef
            .chunks_exact(4)
            .any(|c| !f32::from_le_bytes(c.try_into().unwrap()).is_finite())
        {
            return None;
        }
        Some(Parser {
            dim,
            classes,
            intercept,
            coef,
        })
    }

    /// Structural validation of a SEC_PARSER section WITHOUT the `'static` binding `from_section`
    /// needs (it borrows the mmap for its coef slice). The builder calls this so a corrupt
    /// `--parser` fails the BUILD instead of being embedded and silently dropped to `None` at open
    /// time. MUST stay in lockstep with `from_section`'s checks above.
    pub fn section_is_valid(data: &[u8]) -> bool {
        if data.len() < 9 || &data[0..4] != b"GPML" {
            return false;
        }
        let Ok(dim_bytes) = data[4..8].try_into() else {
            return false;
        };
        let dim = u32::from_le_bytes(dim_bytes) as usize;
        let classes = data[8] as usize;
        if dim == 0 || classes == 0 {
            return false;
        }
        let mut p = 9;
        for _ in 0..classes {
            let Some(sl) = data.get(p..p + 4) else {
                return false;
            };
            if !f32::from_le_bytes(sl.try_into().unwrap()).is_finite() {
                return false;
            }
            p += 4;
        }
        let Some(coef) = data.get(p..) else {
            return false;
        };
        let Some(expected) = classes.checked_mul(dim).and_then(|x| x.checked_mul(4)) else {
            return false;
        };
        coef.len() == expected
            && coef
                .chunks_exact(4)
                .all(|c| f32::from_le_bytes(c.try_into().unwrap()).is_finite())
    }

    fn h(&self, s: &str) -> usize {
        let mut hasher = Blake2bVar::new(4).expect("blake2b-4");
        hasher.update(s.as_bytes());
        let mut out = [0u8; 4];
        hasher.finalize_variable(&mut out).expect("blake2b-4");
        u32::from_le_bytes(out) as usize % self.dim
    }

    fn coef_at(&self, class: usize, idx: usize) -> f32 {
        let off = (class * self.dim + idx) * 4;
        f32::from_le_bytes(self.coef[off..off + 4].try_into().unwrap())
    }

    fn chars_take(s: &str, n: usize) -> String {
        s.chars().take(n).collect()
    }

    fn chars_last(s: &str, n: usize) -> String {
        let cnt = s.chars().count();
        s.chars().skip(cnt.saturating_sub(n)).collect()
    }

    /// class label for each token
    pub fn label(&self, toks: &[&str]) -> Vec<u8> {
        let n = toks.len();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let t = toks[i];
            let prev = if i > 0 { toks[i - 1] } else { "^" };
            let nxt = if i + 1 < n { toks[i + 1] } else { "$" };
            let all_digit = !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
            let any_digit = t.chars().any(|c| c.is_ascii_digit());
            let tn = t.chars().count();
            let dig = if all_digit {
                "1"
            } else if any_digit {
                "m"
            } else {
                "0"
            };
            let pos = if i == 0 {
                "a"
            } else if i == n - 1 {
                "z"
            } else {
                "m"
            };
            let d5 = if all_digit && tn == 5 { "1" } else { "0" };
            let nxt_alpha2 = nxt.chars().count() == 2 && nxt.chars().all(|c| c.is_alphabetic());
            let d4n2 = if all_digit && tn == 4 && i + 1 < n && nxt_alpha2 {
                "1"
            } else {
                "0"
            };
            let pdig = if !prev.is_empty() && prev.chars().all(|c| c.is_ascii_digit()) {
                "1"
            } else {
                "0"
            };
            let feats = [
                format!("w={t}"),
                format!("p={prev}"),
                format!("n={nxt}"),
                format!("sfx={}", Self::chars_last(t, 3)),
                format!("pfx={}", Self::chars_take(t, 3)),
                format!("dig={dig}"),
                format!("len={}", tn.min(7)),
                format!("pos={pos}"),
                format!("d5={d5}"),
                format!("d4n2={d4n2}"),
                format!("pdig={pdig}"),
            ];
            let idxs: Vec<usize> = feats.iter().map(|f| self.h(f)).collect();
            let mut best = (f32::MIN, 0u8);
            for c in 0..self.classes {
                let mut s = self.intercept[c];
                for &ix in &idxs {
                    s += self.coef_at(c, ix);
                }
                if s > best.0 {
                    best = (s, c as u8);
                }
            }
            out.push(best.1);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(dim: u32, classes: u8, coef_len: usize) -> &'static [u8] {
        let mut v = b"GPML".to_vec();
        v.extend_from_slice(&dim.to_le_bytes());
        v.push(classes);
        v.extend(std::iter::repeat_n(0u8, classes as usize * 4)); // intercepts
        v.extend(std::iter::repeat_n(0u8, coef_len));
        Box::leak(v.into_boxed_slice())
    }

    #[test]
    fn zero_dim_is_rejected() {
        // dim == 0 used to pass validation and panic on `% 0` at first query
        assert!(Parser::from_section(section(0, 3, 0)).is_none());
    }

    #[test]
    fn zero_classes_is_rejected() {
        assert!(Parser::from_section(section(16, 0, 16 * 4)).is_none());
    }

    #[test]
    fn nonfinite_weights_are_rejected() {
        // a NaN/Inf intercept or coefficient must fail to load, not embed and
        // silently corrupt every score. Build a valid-shape section then poison one value.
        let poison = |offset: usize, val: f32| -> bool {
            let mut v = b"GPML".to_vec();
            v.extend_from_slice(&2u32.to_le_bytes()); // dim=2
            v.push(2); // classes=2
            v.extend_from_slice(&0f32.to_le_bytes()); // intercept 0
            v.extend_from_slice(&0f32.to_le_bytes()); // intercept 1
            v.extend_from_slice(&[0u8; 2 * 2 * 4]); // coefs (classes*dim*4)
            v[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
            Parser::from_section(Box::leak(v.into_boxed_slice())).is_none()
        };
        assert!(poison(9, f32::NAN), "a NaN intercept must be rejected");
        assert!(
            poison(17, f32::INFINITY),
            "a non-finite coefficient must be rejected"
        );
    }

    #[test]
    fn truncated_intercepts_do_not_panic() {
        // header claims 200 classes but the section is nearly empty
        let mut v = b"GPML".to_vec();
        v.extend_from_slice(&16u32.to_le_bytes());
        v.push(200);
        v.extend_from_slice(&[0u8; 8]);
        assert!(Parser::from_section(Box::leak(v.into_boxed_slice())).is_none());
    }

    /// A model whose weights actually discriminate: class L_NUM gets a positive weight
    /// in every hash bucket, so its score (sum over the 11 hashed features) beats every
    /// other class for ANY token. Proves argmax picks the max class (not trivially 0) and
    /// coef_at indexes/sums correctly — the previous test used all-zero weights, so argmax
    /// returned class 0 no matter what.
    fn discriminating_section(dim: u32, classes: u8, hot_class: u8) -> &'static [u8] {
        let mut v = b"GPML".to_vec();
        v.extend_from_slice(&dim.to_le_bytes());
        v.push(classes);
        v.extend(std::iter::repeat_n(0u8, classes as usize * 4)); // intercepts = 0
        for c in 0..classes {
            for _ in 0..dim {
                let w: f32 = if c == hot_class { 1.0 } else { 0.0 };
                v.extend_from_slice(&w.to_le_bytes());
            }
        }
        Box::leak(v.into_boxed_slice())
    }

    #[test]
    fn argmax_picks_the_weighted_class_not_trivially_zero() {
        let p = Parser::from_section(discriminating_section(16, 5, L_NUM)).expect("valid");
        // every token is labelled L_NUM because that class carries all the weight
        assert_eq!(p.label(&["12", "rue", "de", "la", "paix"]), vec![L_NUM; 5]);
        // and a different hot class shifts the argmax accordingly
        let p2 = Parser::from_section(discriminating_section(16, 5, L_CITY)).expect("valid");
        assert_eq!(p2.label(&["12", "rue"]), vec![L_CITY, L_CITY]);
    }

    #[test]
    fn tiny_valid_model_loads_and_labels() {
        let p = Parser::from_section(section(4, 2, 2 * 4 * 4)).expect("valid section");
        assert_eq!(p.label(&["12", "rue"]).len(), 2);
    }
}
