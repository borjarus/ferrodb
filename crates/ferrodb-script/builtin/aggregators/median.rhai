// name: Median
// description: Median of the numeric values

fn aggregate(values) {
    let xs = numbers(values);
    let n = xs.len();
    if n == 0 {
        return ();
    }
    xs.sort();
    if n % 2 == 1 { xs[n / 2] } else { round_to((xs[n / 2 - 1] + xs[n / 2]) / 2.0, 10) }
}
