// name: SQL IN list
// description: The first column as an SQL IN list: (1, 2, 'x')
// extension: sql

fn header() {
    "("
}

fn row(values) {
    if values.is_empty() { "NULL" } else { sql_literal(values[0]) }
}

fn separator() {
    ", "
}

fn footer() {
    ")"
}
