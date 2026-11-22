// name: Markdown (aligned)
// description: A Markdown table with padded columns (keeps the rows until the end)
// extension: md

fn header(columns) {
    this.names = columns.map(|c| md_escape(c.name));
    this.right = columns.map(|c| c.type == "integer" || c.type == "float" || c.type == "decimal");
    this.rows = [];
    ""
}

fn row(values) {
    let nt = options().null_text;
    this.rows.push(values.map(|v| md_escape(text(v, nt))));
    ""
}

fn line(cells, widths, right) {
    let parts = [];
    for i in 0..widths.len() {
        let cell = if i < cells.len() { cells[i] } else { "" };
        parts.push(if right[i] { pad_left(cell, widths[i]) } else { pad(cell, widths[i]) });
    }
    "| " + join(parts, " | ") + " |\n"
}

fn footer() {
    let n = this.names.len();
    if n == 0 {
        return "";
    }
    let widths = this.names.map(|s| max(s.len(), 3));
    for r in this.rows {
        for i in 0..n {
            if i < r.len() {
                widths[i] = max(widths[i], r[i].len());
            }
        }
    }
    let dashes = [];
    for i in 0..n {
        let d = "";
        if this.right[i] {
            d.pad(widths[i] - 1, '-');
            d += ":";
        } else {
            d.pad(widths[i], '-');
        }
        dashes.push(d);
    }
    let out = line(this.names, widths, this.right) + "| " + join(dashes, " | ") + " |\n";
    for r in this.rows {
        out += line(r, widths, this.right);
    }
    out
}
