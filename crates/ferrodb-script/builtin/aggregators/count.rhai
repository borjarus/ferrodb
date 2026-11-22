// name: Count
// description: Number of non-NULL values

fn aggregate(values) {
    values.filter(|v| !is_null(v)).len()
}
