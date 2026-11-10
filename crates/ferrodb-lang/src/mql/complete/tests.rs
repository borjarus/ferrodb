//! Suggestion accuracy corpus (plan, phase 4b: ≥ 90 %). Each case is a
//! console text with `|` at the cursor and the label that must be among
//! the first three suggestions.

use super::*;

struct Shop;

impl Schema for Shop {
    fn collections(&self) -> Vec<String> {
        [
            "orders",
            "customers",
            "products",
            "order_items",
            "system.views",
        ]
        .map(String::from)
        .to_vec()
    }
    fn fields(&self, collection: &str) -> Vec<String> {
        let f: &[&str] = match collection {
            "orders" => &[
                "_id",
                "status",
                "total",
                "customerId",
                "createdAt",
                "items",
                "items.sku",
                "items.qty",
                "shipping.city",
            ],
            "customers" => &[
                "_id",
                "name",
                "email",
                "address",
                "address.city",
                "address.zip",
                "tags",
                "active",
            ],
            "products" => &["_id", "sku", "name", "price", "stock", "category"],
            _ => &["_id"],
        };
        f.iter().map(|s| s.to_string()).collect()
    }
}

const CORPUS: &[(&str, &str)] = &[
    ("|", "db"),
    ("d|", "db"),
    ("us|", "use"),
    ("db.|", "orders"),
    ("db.ord|", "orders"),
    ("db.cus|", "customers"),
    ("db.order_|", "order_items"),
    ("db.getCollectionN|", "getCollectionNames"),
    ("db.runC|", "runCommand"),
    ("db.orders.|", "find"),
    ("db.orders.fi|", "find"),
    ("db.orders.findOne|", "findOne"),
    ("db.orders.agg|", "aggregate"),
    ("db.orders.countD|", "countDocuments"),
    ("db.orders.insertM|", "insertMany"),
    ("db.orders.updateO|", "updateOne"),
    ("db.orders.deleteM|", "deleteMany"),
    ("db.orders.createI|", "createIndex"),
    ("db.orders.getI|", "getIndexes"),
    ("db.orders.find({}).|", "sort"),
    ("db.orders.find({}).li|", "limit"),
    ("db.orders.find({}).sort({total: -1}).sk|", "skip"),
    ("db.orders.find({st|", "status"),
    ("db.orders.find({ status: 'A', to|", "total"),
    ("db.orders.find({ cust|", "customerId"),
    ("db.orders.find({ total: { $g|", "$gt"),
    ("db.orders.find({ total: { $gte: 5, $l|", "$lt"),
    ("db.orders.find({ status: { $i|", "$in"),
    ("db.orders.find({ $o|", "$or"),
    ("db.orders.find({ $or: [{ st|", "status"),
    ("db.orders.find({ $and: [{ total: { $e|", "$exists"),
    ("db.orders.find({ items: { $elemMatch: { sk|", "items.sku"),
    ("db.orders.find({}, { tot|", "total"),
    ("db.orders.find({}).sort({ crea|", "createdAt"),
    ("db.customers.find({ 'address.c|", "address.city"),
    ("db.customers.find({ addr|", "address"),
    ("db.customers.find({ address.ci|", "address.city"),
    ("db.customers.find({ em|", "email"),
    ("db.customers.find({ active: tr|", "true"),
    ("db.customers.find({ _id: Obj|", "ObjectId(\"\")"),
    ("db.orders.find({ createdAt: { $gt: ISO|", "ISODate(\"\")"),
    ("db.orders.updateOne({ _id: 1 }, { $s|", "$set"),
    ("db.orders.updateOne({ _id: 1 }, { $in|", "$inc"),
    ("db.orders.updateMany({}, { $set: { sta|", "status"),
    ("db.orders.updateOne({ st|", "status"),
    ("db.orders.updateOne({}, { $pu|", "$push"),
    ("db.orders.deleteOne({ stat|", "status"),
    ("db.orders.countDocuments({ to|", "total"),
    ("db.orders.aggregate([{ $m|", "$match"),
    ("db.orders.aggregate([{ $gr|", "$group"),
    ("db.orders.aggregate([{ $match: {} }, { $lo|", "$lookup"),
    ("db.orders.aggregate([{ $match: { sta|", "status"),
    ("db.orders.aggregate([{ $match: { total: { $g|", "$gt"),
    ("db.orders.aggregate([{ $group: { _id: '$st|", "$status"),
    (
        "db.orders.aggregate([{ $group: { _id: '$status', n: { $su|",
        "$sum",
    ),
    (
        "db.orders.aggregate([{ $group: { _id: null, avg: { $avg: '$tot|",
        "$total",
    ),
    ("db.orders.aggregate([{ $project: { tot|", "total"),
    ("db.orders.aggregate([{ $sort: { crea|", "createdAt"),
    ("db.orders.aggregate([{ $unw|", "$unwind"),
    ("db.orders.createIndex({ cust|", "customerId"),
    ("db.orders.distinct('sta|", "status"),
    ("db.getCollection('ord|", "orders"),
    ("db.getCollection('orders').fi|", "find"),
    ("db.getCollection('orders').find({ to|", "total"),
    ("db['products'].find({ pr|", "price"),
    (
        "db.products.find({ price: { $lt: 10 } }).sort({ na|",
        "name",
    ),
    (
        "db.products.find({\n  stock: { $lte: 0 },\n  cat|",
        "category",
    ),
    ("db.products.find({ category: 'x' })\n  .lim|", "limit"),
    ("db.orders.find({});\ndb.cus|", "customers"),
    ("db.orders.find({})\ndb.products.find({ sk|", "sku"),
    ("show dbs\n|", "db"),
    (
        "db.orders.findOneAndUpdate({ status: 'new' }, { $set: { stat|",
        "status",
    ),
    ("db.orders.replaceOne({ _id: 1 }, { sta|", "status"),
    ("db.orders.find({ shipping.ci|", "shipping.city"),
];

#[test]
fn suggestion_accuracy_on_the_corpus() {
    let mut misses = Vec::new();
    for (case, expected) in CORPUS {
        let offset = case.find('|').unwrap();
        let text = case.replace('|', "");
        let (range, items) = complete(&text, offset, &Shop);
        let top: Vec<&str> = items.iter().take(3).map(|s| s.label.as_str()).collect();
        if !top.contains(expected) {
            misses.push(format!("{case:?}: expected {expected:?}, got {top:?}"));
        }
        assert!(range.end == offset && range.start <= offset);
    }
    let accuracy = 1.0 - misses.len() as f64 / CORPUS.len() as f64;
    println!(
        "MQL suggestion accuracy: {:.1} % ({} cases)\n{}",
        accuracy * 100.0,
        CORPUS.len(),
        misses.join("\n")
    );
    assert!(
        accuracy >= 0.9,
        "accuracy {accuracy:.3} below 90 %:\n{}",
        misses.join("\n")
    );
}

#[test]
fn replace_range_covers_the_prefix() {
    let (r, items) = complete("db.ord", 6, &Shop);
    assert_eq!(r, 3..6);
    assert_eq!(items[0].label, "orders");
    assert_eq!(items[0].kind, CompletionKind::Collection);
}
