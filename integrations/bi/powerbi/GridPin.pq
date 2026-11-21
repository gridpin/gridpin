// GridPin.Geocode for Power BI Desktop (Power Query M): adds gp_lat, gp_lon, gp_label and gp_weak to a table by asking
// a GridPin server on the same machine (gridpin serve sheet.bin). Addresses go in the body of a POST, 1000 per request.
// Paste as a blank query named GridPin.Geocode, then: = GridPin.Geocode(Source, "Address")
// Power BI Service cannot reach 127.0.0.1: a scheduled refresh needs an on-premises data gateway on that machine.
(source as table, column as text, optional server as nullable text) as table =>
let
    Server = if server = null then "http://127.0.0.1:8080/" else server,
    Rows = Table.ToRecords(source),
    Addresses = List.Transform(Rows, each let v = Record.Field(_, column) in if v = null then "" else Text.Trim(Text.From(v))),
    Batches = List.Split(Addresses, 1000),
    Ask = (batch as list) as list =>
        let
            Raw = Web.Contents(Server, [
                RelativePath = "v1/geocode",
                Content = Json.FromValue([queries = batch, k = 1]),
                Headers = [#"Content-Type" = "application/json"],
                ManualStatusHandling = {400, 500, 503}
            ]),
            Status = Value.Metadata(Raw)[Response.Status],
            Answer = if Status <> 200
                then error Error.Record("GridPin", "The GridPin server at " & Server & " answered " & Text.From(Status))
                else Json.Document(Raw)
        in
            Answer[results],
    Results = List.Combine(List.Transform(Batches, Ask)),
    Best = List.Transform(Results, each if List.IsEmpty(_) then null else _{0}),
    Label = (h as record) as text =>
        Text.Combine(List.Select({
            Text.Combine(List.Select({Record.FieldOrDefault(h, "housenumber", null), h[street]}, each _ <> null and _ <> ""), " "),
            h[commune]}, each _ <> null and _ <> ""), ", "),
    Added = Table.FromRecords(List.Transform(List.Zip({Rows, Best}), (pair) =>
        Record.Combine({pair{0}, [
            gp_lat = if pair{1} = null then null else pair{1}[lat],
            gp_lon = if pair{1} = null then null else pair{1}[lon],
            gp_label = if pair{1} = null then null else Label(pair{1}),
            gp_weak = if pair{1} = null then null else pair{1}[weak]
        ]})))
in
    Table.TransformColumnTypes(Added, {{"gp_lat", type number}, {"gp_lon", type number}, {"gp_label", type text}, {"gp_weak", type logical}})
