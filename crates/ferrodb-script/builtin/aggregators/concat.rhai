// name: Comma-joined
// description: The non-NULL values joined with ", "

fn aggregate(values) {
    join(values.filter(|v| !is_null(v)), ", ")
}
