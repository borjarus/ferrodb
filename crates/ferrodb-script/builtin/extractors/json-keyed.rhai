// name: JSON object keyed by first column
// description: {"<first column>": {<other columns>}, …}
// extension: json

fn header() {
    "{"
}

fn row(values, columns) {
    let fields = [];
    for i in 1..values.len() {
        fields.push(json(columns[i].name) + ": " + json(values[i]));
    }
    "\n  " + json(text(values[0], "null")) + ": {" + join(fields, ", ") + "}"
}

fn separator() {
    ","
}

fn footer(count) {
    if count > 0 { "\n}\n" } else { "}\n" }
}
