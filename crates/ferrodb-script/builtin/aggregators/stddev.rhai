// name: Standard deviation
// description: Sample standard deviation of the numeric values

fn aggregate(values) {
    let xs = numbers(values);
    let n = xs.len();
    if n < 2 {
        return ();
    }
    let sum = 0.0;
    for x in xs {
        sum += x;
    }
    let mean = sum / n.to_float();
    let ss = 0.0;
    for x in xs {
        ss += (x - mean) * (x - mean);
    }
    round_to(sqrt(ss / (n - 1).to_float()), 10)
}
