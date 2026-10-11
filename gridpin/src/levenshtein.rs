// Adapted from fst 0.4.7 and utf8-ranges 1.0.5 by Andrew Gallant.
// Both are dual licensed MIT / Unlicense; this copy uses MIT.
// The MIT License (MIT)
//
// Copyright (c) 2015 Andrew Gallant
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.
use std::cmp;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt;

use self::utf8_ranges::{Utf8Range, Utf8Sequences};

use fst::Automaton;

const DEFAULT_STATE_LIMIT: usize = 10_000; // currently at least 20MB >_<

#[derive(Debug)]
pub enum LevenshteinError {
    TooManyStates(usize),
}

impl fmt::Display for LevenshteinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            LevenshteinError::TooManyStates(size_limit) => write!(
                f,
                "Levenshtein automaton exceeds size limit of \
                           {} states",
                size_limit
            ),
        }
    }
}

impl std::error::Error for LevenshteinError {}

pub struct Levenshtein {
    prog: DynamicLevenshtein,
    dfa: Dfa,
}

impl Levenshtein {
    pub fn new(query: &str, distance: u32) -> Result<Levenshtein, LevenshteinError> {
        let lev = DynamicLevenshtein {
            query: query.to_owned(),
            dist: distance as usize,
        };
        let dfa = DfaBuilder::new(lev.clone()).build()?;
        Ok(Levenshtein { prog: lev, dfa })
    }
}

impl fmt::Debug for Levenshtein {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Levenshtein(query: {:?}, distance: {:?})",
            self.prog.query, self.prog.dist
        )
    }
}

#[derive(Clone)]
struct DynamicLevenshtein {
    query: String,
    dist: usize,
}

impl DynamicLevenshtein {
    fn start(&self) -> Vec<usize> {
        (0..self.query.chars().count() + 1).collect()
    }

    fn is_match(&self, state: &[usize]) -> bool {
        state.last().map(|&n| n <= self.dist).unwrap_or(false)
    }

    fn can_match(&self, state: &[usize]) -> bool {
        state.iter().min().map(|&n| n <= self.dist).unwrap_or(false)
    }

    fn accept(&self, state: &[usize], chr: Option<char>) -> Vec<usize> {
        let mut next = vec![state[0] + 1];
        for (i, c) in self.query.chars().enumerate() {
            let cost = if Some(c) == chr { 0 } else { 1 };
            let v = cmp::min(cmp::min(next[i] + 1, state[i + 1] + 1), state[i] + cost);
            next.push(cmp::min(v, self.dist + 1));
        }
        next
    }
}

impl Automaton for Levenshtein {
    type State = Option<usize>;

    #[inline]
    fn start(&self) -> Option<usize> {
        Some(0)
    }

    #[inline]
    fn is_match(&self, state: &Option<usize>) -> bool {
        state
            .map(|state| self.dfa.states[state].is_match)
            .unwrap_or(false)
    }

    #[inline]
    fn can_match(&self, state: &Option<usize>) -> bool {
        state.is_some()
    }

    #[inline]
    fn accept(&self, state: &Option<usize>, byte: u8) -> Option<usize> {
        state.and_then(|state| {
            let next = self.dfa.states[state].next[byte as usize];
            (next != u32::MAX).then_some(next as usize)
        })
    }
}

#[derive(Debug)]
struct Dfa {
    states: StateChunks,
}

struct State {
    next: [u32; 256],
    is_match: bool,
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "State {{")?;
        writeln!(f, "  is_match: {:?}", self.is_match)?;
        for i in 0..256 {
            let si = self.next[i];
            if si != u32::MAX {
                writeln!(f, "  {:?}: {:?}", i, si)?;
            }
        }
        write!(f, "}}")
    }
}

struct DfaBuilder {
    dfa: Dfa,
    lev: DynamicLevenshtein,
    cache: HashMap<Vec<usize>, usize>,
}

impl DfaBuilder {
    fn new(lev: DynamicLevenshtein) -> DfaBuilder {
        DfaBuilder {
            dfa: Dfa {
                states: StateChunks::new(),
            },
            lev,
            cache: HashMap::with_capacity(1024),
        }
    }

    fn build_with_limit(mut self, state_limit: usize) -> Result<Dfa, LevenshteinError> {
        let mut stack = vec![self.lev.start()];
        let mut seen = HashSet::new();
        let query = self.lev.query.clone(); // temp work around of borrowck
        while let Some(lev_state) = stack.pop() {
            let dfa_si = self.cached_state(&lev_state).unwrap();
            let mismatch = self.add_mismatch_utf8_states(dfa_si, &lev_state);
            if let Some((next_si, lev_next)) = mismatch {
                if !seen.contains(&next_si) {
                    seen.insert(next_si);
                    stack.push(lev_next);
                }
            }
            for (i, c) in query.chars().enumerate() {
                if lev_state[i] > self.lev.dist {
                    continue;
                }
                let lev_next = self.lev.accept(&lev_state, Some(c));
                let next_si = self.cached_state(&lev_next);
                if let Some(next_si) = next_si {
                    self.add_utf8_sequences(true, dfa_si, next_si, c, c);
                    if !seen.contains(&next_si) {
                        seen.insert(next_si);
                        stack.push(lev_next);
                    }
                }
            }
            if self.dfa.states.len() > state_limit {
                return Err(LevenshteinError::TooManyStates(state_limit));
            }
        }
        Ok(self.dfa)
    }

    fn build(self) -> Result<Dfa, LevenshteinError> {
        self.build_with_limit(DEFAULT_STATE_LIMIT)
    }

    fn cached_state(&mut self, lev_state: &[usize]) -> Option<usize> {
        self.cached(lev_state).map(|(si, _)| si)
    }

    fn cached(&mut self, lev_state: &[usize]) -> Option<(usize, bool)> {
        if !self.lev.can_match(lev_state) {
            return None;
        }
        Some(match self.cache.entry(lev_state.to_vec()) {
            Entry::Occupied(v) => (*v.get(), true),
            Entry::Vacant(v) => {
                let is_match = self.lev.is_match(lev_state);
                self.dfa.states.push(State {
                    next: [u32::MAX; 256],
                    is_match,
                });
                (*v.insert(self.dfa.states.len() - 1), false)
            }
        })
    }

    fn add_mismatch_utf8_states(
        &mut self,
        from_si: usize,
        lev_state: &[usize],
    ) -> Option<(usize, Vec<usize>)> {
        let mismatch_state = self.lev.accept(lev_state, None);
        let to_si = match self.cached(&mismatch_state) {
            None => return None,
            Some((si, _)) => si,
            // Some((si, true)) => return Some((si, mismatch_state)),
            // Some((si, false)) => si,
        };
        self.add_utf8_sequences(false, from_si, to_si, '\u{0}', '\u{10FFFF}');
        Some((to_si, mismatch_state))
    }

    fn add_utf8_sequences(
        &mut self,
        overwrite: bool,
        from_si: usize,
        to_si: usize,
        from_chr: char,
        to_chr: char,
    ) {
        for seq in Utf8Sequences::new(from_chr, to_chr) {
            let mut fsi = from_si;
            for range in &seq.as_slice()[0..seq.len() - 1] {
                let tsi = self.new_state(false);
                self.add_utf8_range(overwrite, fsi, tsi, range);
                fsi = tsi;
            }
            self.add_utf8_range(overwrite, fsi, to_si, &seq.as_slice()[seq.len() - 1]);
        }
    }

    fn add_utf8_range(&mut self, overwrite: bool, from: usize, to: usize, range: &Utf8Range) {
        for b in range.start as usize..range.end as usize + 1 {
            if overwrite || self.dfa.states[from].next[b] == u32::MAX {
                // The fixed 10,000-state limit leaves the sentinel unreachable.
                self.dfa.states[from].next[b] = u32::try_from(to).expect("state index fits u32");
            }
        }
    }

    fn new_state(&mut self, is_match: bool) -> usize {
        self.dfa.states.push(State {
            next: [u32::MAX; 256],
            is_match,
        });
        self.dfa.states.len() - 1
    }
}
// Unmodified UTF-8 range traversal from utf8-ranges 1.0.5.
// Unused public upstream helpers remain here to keep the copied algorithm auditable.
#[allow(dead_code)]
mod utf8_ranges {
    use std::char;
    use std::fmt;
    use std::slice;

    use self::char_utf8::encode_utf8;

    const MAX_UTF8_BYTES: usize = 4;

    mod char_utf8 {
        // Pulled from std::char until encode_utf8 stabilizes. ---AG

        // UTF-8 ranges and tags for encoding characters
        const TAG_CONT: u8 = 0b1000_0000;
        const TAG_TWO_B: u8 = 0b1100_0000;
        const TAG_THREE_B: u8 = 0b1110_0000;
        const TAG_FOUR_B: u8 = 0b1111_0000;
        const MAX_ONE_B: u32 = 0x80;
        const MAX_TWO_B: u32 = 0x800;
        const MAX_THREE_B: u32 = 0x10000;

        #[inline]
        pub fn encode_utf8(character: char, dst: &mut [u8]) -> Option<usize> {
            let code = character as u32;
            if code < MAX_ONE_B && !dst.is_empty() {
                dst[0] = code as u8;
                Some(1)
            } else if code < MAX_TWO_B && dst.len() >= 2 {
                dst[0] = (code >> 6 & 0x1F) as u8 | TAG_TWO_B;
                dst[1] = (code & 0x3F) as u8 | TAG_CONT;
                Some(2)
            } else if code < MAX_THREE_B && dst.len() >= 3 {
                dst[0] = (code >> 12 & 0x0F) as u8 | TAG_THREE_B;
                dst[1] = (code >> 6 & 0x3F) as u8 | TAG_CONT;
                dst[2] = (code & 0x3F) as u8 | TAG_CONT;
                Some(3)
            } else if dst.len() >= 4 {
                dst[0] = (code >> 18 & 0x07) as u8 | TAG_FOUR_B;
                dst[1] = (code >> 12 & 0x3F) as u8 | TAG_CONT;
                dst[2] = (code >> 6 & 0x3F) as u8 | TAG_CONT;
                dst[3] = (code & 0x3F) as u8 | TAG_CONT;
                Some(4)
            } else {
                None
            }
        }
    }

    #[derive(Copy, Clone, Eq, PartialEq)]
    pub enum Utf8Sequence {
        One(Utf8Range),
        Two([Utf8Range; 2]),
        Three([Utf8Range; 3]),
        Four([Utf8Range; 4]),
    }

    impl Utf8Sequence {
        fn from_encoded_range(start: &[u8], end: &[u8]) -> Self {
            assert_eq!(start.len(), end.len());
            match start.len() {
                2 => Utf8Sequence::Two([
                    Utf8Range::new(start[0], end[0]),
                    Utf8Range::new(start[1], end[1]),
                ]),
                3 => Utf8Sequence::Three([
                    Utf8Range::new(start[0], end[0]),
                    Utf8Range::new(start[1], end[1]),
                    Utf8Range::new(start[2], end[2]),
                ]),
                4 => Utf8Sequence::Four([
                    Utf8Range::new(start[0], end[0]),
                    Utf8Range::new(start[1], end[1]),
                    Utf8Range::new(start[2], end[2]),
                    Utf8Range::new(start[3], end[3]),
                ]),
                n => unreachable!("invalid encoded length: {}", n),
            }
        }

        pub fn as_slice(&self) -> &[Utf8Range] {
            use self::Utf8Sequence::*;
            match *self {
                One(ref r) => slice::from_ref(r),
                Two(ref r) => &r[..],
                Three(ref r) => &r[..],
                Four(ref r) => &r[..],
            }
        }

        pub fn len(&self) -> usize {
            self.as_slice().len()
        }

        pub fn matches(&self, bytes: &[u8]) -> bool {
            if bytes.len() < self.len() {
                return false;
            }
            for (&b, r) in bytes.iter().zip(self) {
                if !r.matches(b) {
                    return false;
                }
            }
            true
        }
    }

    impl<'a> IntoIterator for &'a Utf8Sequence {
        type IntoIter = slice::Iter<'a, Utf8Range>;
        type Item = &'a Utf8Range;

        fn into_iter(self) -> Self::IntoIter {
            self.as_slice().iter()
        }
    }

    impl fmt::Debug for Utf8Sequence {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            use self::Utf8Sequence::*;
            match *self {
                One(ref r) => write!(f, "{:?}", r),
                Two(ref r) => write!(f, "{:?}{:?}", r[0], r[1]),
                Three(ref r) => write!(f, "{:?}{:?}{:?}", r[0], r[1], r[2]),
                Four(ref r) => write!(f, "{:?}{:?}{:?}{:?}", r[0], r[1], r[2], r[3]),
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct Utf8Range {
        pub start: u8,
        pub end: u8,
    }

    impl Utf8Range {
        fn new(start: u8, end: u8) -> Self {
            Utf8Range { start, end }
        }

        pub fn matches(&self, b: u8) -> bool {
            self.start <= b && b <= self.end
        }
    }

    impl fmt::Debug for Utf8Range {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            if self.start == self.end {
                write!(f, "[{:X}]", self.start)
            } else {
                write!(f, "[{:X}-{:X}]", self.start, self.end)
            }
        }
    }

    pub struct Utf8Sequences {
        range_stack: Vec<ScalarRange>,
    }

    impl Utf8Sequences {
        pub fn new(start: char, end: char) -> Self {
            let mut it = Utf8Sequences {
                range_stack: vec![],
            };
            it.push(start as u32, end as u32);
            it
        }

        #[doc(hidden)]
        pub fn reset(&mut self, start: char, end: char) {
            self.range_stack.clear();
            self.push(start as u32, end as u32);
        }

        fn push(&mut self, start: u32, end: u32) {
            self.range_stack.push(ScalarRange { start, end });
        }
    }

    struct ScalarRange {
        start: u32,
        end: u32,
    }

    impl fmt::Debug for ScalarRange {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "ScalarRange({:X}, {:X})", self.start, self.end)
        }
    }

    impl Iterator for Utf8Sequences {
        type Item = Utf8Sequence;

        fn next(&mut self) -> Option<Self::Item> {
            'TOP: while let Some(mut r) = self.range_stack.pop() {
                'INNER: loop {
                    if let Some((r1, r2)) = r.split() {
                        self.push(r2.start, r2.end);
                        r.start = r1.start;
                        r.end = r1.end;
                        continue 'INNER;
                    }
                    if !r.is_valid() {
                        continue 'TOP;
                    }
                    for i in 1..MAX_UTF8_BYTES {
                        let max = max_scalar_value(i);
                        if r.start <= max && max < r.end {
                            self.push(max + 1, r.end);
                            r.end = max;
                            continue 'INNER;
                        }
                    }
                    if let Some(ascii_range) = r.as_ascii() {
                        return Some(Utf8Sequence::One(ascii_range));
                    }
                    for i in 1..MAX_UTF8_BYTES {
                        let m = (1 << (6 * i)) - 1;
                        if (r.start & !m) != (r.end & !m) {
                            if (r.start & m) != 0 {
                                self.push((r.start | m) + 1, r.end);
                                r.end = r.start | m;
                                continue 'INNER;
                            }
                            if (r.end & m) != m {
                                self.push(r.end & !m, r.end);
                                r.end = (r.end & !m) - 1;
                                continue 'INNER;
                            }
                        }
                    }
                    let mut start = [0; MAX_UTF8_BYTES];
                    let mut end = [0; MAX_UTF8_BYTES];
                    let n = r.encode(&mut start, &mut end);
                    return Some(Utf8Sequence::from_encoded_range(&start[0..n], &end[0..n]));
                }
            }
            None
        }
    }

    impl ScalarRange {
        fn split(&self) -> Option<(ScalarRange, ScalarRange)> {
            if self.start < 0xE000 && self.end > 0xD7FF {
                Some((
                    ScalarRange {
                        start: self.start,
                        end: 0xD7FF,
                    },
                    ScalarRange {
                        start: 0xE000,
                        end: self.end,
                    },
                ))
            } else {
                None
            }
        }

        fn is_valid(&self) -> bool {
            self.start <= self.end
        }

        fn as_ascii(&self) -> Option<Utf8Range> {
            if self.is_ascii() {
                Some(Utf8Range::new(self.start as u8, self.end as u8))
            } else {
                None
            }
        }

        fn is_ascii(&self) -> bool {
            self.is_valid() && self.end <= 0x7f
        }

        fn encode(&self, start: &mut [u8], end: &mut [u8]) -> usize {
            let cs = char::from_u32(self.start).unwrap();
            let ce = char::from_u32(self.end).unwrap();
            let n = encode_utf8(cs, start).unwrap();
            let m = encode_utf8(ce, end).unwrap();
            assert_eq!(n, m);
            n
        }
    }

    fn max_scalar_value(nbytes: usize) -> u32 {
        match nbytes {
            1 => 0x007F,
            2 => 0x07FF,
            3 => 0xFFFF,
            4 => 0x10FFFF,
            _ => unreachable!("invalid UTF-8 byte sequence size"),
        }
    }
}

// State objects never move when the collection grows. Only the small outer
// vector of chunk descriptors can be reallocated. The last chunk is lazy.
#[derive(Debug)]
struct StateChunks {
    chunks: Vec<Vec<State>>,
    len: usize,
}
impl StateChunks {
    const CHUNK: usize = 64;
    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }
    fn len(&self) -> usize {
        self.len
    }
    fn push(&mut self, state: State) {
        if self.len.is_multiple_of(Self::CHUNK) {
            self.chunks.push(Vec::with_capacity(Self::CHUNK));
        }
        self.chunks.last_mut().unwrap().push(state);
        self.len += 1;
    }
}
impl std::ops::Index<usize> for StateChunks {
    type Output = State;
    fn index(&self, i: usize) -> &State {
        &self.chunks[i / Self::CHUNK][i % Self::CHUNK]
    }
}
impl std::ops::IndexMut<usize> for StateChunks {
    fn index_mut(&mut self, i: usize) -> &mut State {
        &mut self.chunks[i / Self::CHUNK][i % Self::CHUNK]
    }
}
