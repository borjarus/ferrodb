// name: Percentile 90
// description: 90th percentile of the numeric values (linear interpolation)

fn aggregate(values) {
    let xs = numbers(values);
    let n = xs.len();
    if n == 0 {
        return ();
    }
    xs.sort();
    let pos = 0.9 * (n - 1).to_float();
    let lo = pos.floor().to_int();
    let frac = pos - lo.to_float();
    let p = if lo + 1 < n { xs[lo] + (xs[lo + 1] - xs[lo]) * frac } else { xs[lo] };
    round_to(p, 10)
}
