// name: Pipe-separated
// description: Values separated by |, header line when enabled
// extension: txt

fn field(v) {
    let s = text(v, options().null_text);
    s.replace("|", "\\|");
    s.replace("\r\n", " ");
    s.replace("\n", " ");
    s
}

fn header(columns) {
    if options().header {
        join(columns.map(|c| field(c.name)), "|") + "\n"
    } else {
        ""
    }
}

fn row(values) {
    join(values.map(|v| field(v)), "|")
}

fn separator() {
    "\n"
}

fn footer(count) {
    if count > 0 { "\n" } else { "" }
}
