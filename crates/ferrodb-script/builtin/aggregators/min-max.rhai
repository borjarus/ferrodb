// name: Min – max
// description: Smallest and largest numeric value

fn aggregate(values) {
    let xs = numbers(values);
    if xs.is_empty() {
        return ();
    }
    let lo = xs[0];
    let hi = xs[0];
    for x in xs {
        if x < lo { lo = x; }
        if x > hi { hi = x; }
    }
    num_text(lo) + " – " + num_text(hi)
}
