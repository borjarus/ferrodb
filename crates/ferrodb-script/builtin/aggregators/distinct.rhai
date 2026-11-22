// name: Distinct count
// description: Number of distinct non-NULL values

fn aggregate(values) {
    let xs = values.filter(|v| !is_null(v)).map(|v| text(v));
    xs.sort();
    xs.dedup();
    xs.len()
}
