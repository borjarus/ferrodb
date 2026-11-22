// name: SQL VALUES rows
// description: Rows as SQL tuples, one per line: (1, 'a'),
// extension: sql

fn row(values) {
    "(" + join(values.map(|v| sql_literal(v)), ", ") + ")"
}

fn separator() {
    ",\n"
}

fn footer(count) {
    if count > 0 { "\n" } else { "" }
}
