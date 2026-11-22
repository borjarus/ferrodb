// name: PHP array
// description: A PHP array of associative arrays (column => value)
// extension: php

fn php(v) {
    if is_null(v) {
        return "null";
    }
    if type_of(v) == "bool" {
        return if v { "true" } else { "false" };
    }
    if is_number(v) {
        return num_text(v);
    }
    let s = text(v);
    s.replace("\\", "\\\\");
    s.replace("'", "\\'");
    "'" + s + "'"
}

fn header() {
    "<?php\nreturn [\n"
}

fn row(values, columns) {
    let parts = [];
    for i in 0..values.len() {
        parts.push(php(columns[i].name) + " => " + php(values[i]));
    }
    "    [" + join(parts, ", ") + "],\n"
}

fn footer() {
    "];\n"
}
