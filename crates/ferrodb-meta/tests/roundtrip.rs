//! DDL round trips on real servers (plan, phase 4: "correct DDL for the
//! test corpus"): create a corpus in `rt_src`, introspect it, generate the
//! DDL of the whole container for `rt_dst`, run it, introspect `rt_dst`
//! and require the same model (container names aside).
//!
//! Servers come from the TCK variables (`cargo xtask db-up` prints them):
//! `FERRODB_TCK_POSTGRES`, `FERRODB_TCK_MYSQL`, `FERRODB_TCK_MARIADB`,
//! `FERRODB_TCK_MONGODB`. Unset variables skip their test.

use ferrodb_drivers::{CancellationToken, Connection, Registry, ResolvedConfig, sink::collect};
use ferrodb_lang::sql::{Flavor, split};
use ferrodb_meta::{
    Content, Dialect,
    ddl::{Target, container_ddl},
    introspector,
};

async fn connect(driver: &str, var: &str) -> Option<Box<dyn Connection>> {
    let url = std::env::var(var).ok()?;
    let cfg = if driver == "mongodb" {
        ResolvedConfig::new("mongodb").with("uri", url)
    } else {
        ResolvedConfig::from_url(driver, &url).unwrap()
    };
    let reg = Registry::builtin();
    Some(
        reg.get(&driver.into())
            .unwrap()
            .connect(&cfg)
            .await
            .unwrap(),
    )
}

async fn run(conn: &mut dyn Connection, dialect: &str, script: &str) {
    for st in split(script, &Flavor::for_dialect(dialect)) {
        let sql = st.text(script);
        collect(conn, sql, CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{e}\n--- statement:\n{sql}\n--- script:\n{script}"));
    }
}

/// The model as JSON with container names unified and volatile parts zeroed.
fn normalized(mut c: Content, container: &str) -> String {
    c.refreshed_at = 0;
    for t in &mut c.tables {
        t.rows = None;
    }
    serde_json::to_string_pretty(&c)
        .unwrap()
        .replace(container, "<container>")
}

async fn round_trip(conn: &mut dyn Connection, d: Dialect, dialect: &str, corpus: &str) {
    let i = introspector(d);
    let src = i.content(conn, "rt_src", None).await.unwrap();
    assert!(!src.tables.is_empty());
    let ddl = container_ddl(
        &Target {
            rename: Some("rt_dst"),
            ..Target::new(d, "rt_src")
        },
        &src,
    );
    if d.is_mysql() {
        conn.set_schema("rt_dst").await.unwrap();
    }
    run(conn, dialect, &ddl).await;
    let dst = i.content(conn, "rt_dst", None).await.unwrap();
    let (a, b) = (normalized(src, "rt_src"), normalized(dst, "rt_dst"));
    if a != b {
        let diff: Vec<String> = a
            .lines()
            .zip(b.lines())
            .enumerate()
            .filter(|(_, (x, y))| x != y)
            .take(20)
            .map(|(n, (x, y))| format!("line {n}:\n  src: {x}\n  dst: {y}"))
            .collect();
        panic!(
            "round trip differs:\n{}\n--- DDL:\n{ddl}\n--- corpus:\n{corpus}",
            diff.join("\n")
        );
    }
    println!(
        "{dialect}: rt_src round-tripped into rt_dst ({} bytes of model)",
        a.len()
    );
}

const PG: &str = r#"
drop schema if exists rt_src cascade;
drop schema if exists rt_dst cascade;
create schema rt_src;
create schema rt_dst;
create sequence rt_src.invoice_no start 1000 increment 5;
create table rt_src.customers (
  id serial primary key,
  email varchar(200) not null unique,
  name text default 'anon',
  created timestamptz default now(),
  "Mixed Case" int,
  constraint email_ck check (email like '%@%')
);
comment on table rt_src.customers is 'People who buy';
comment on column rt_src.customers.email is 'Login, it''s unique';
create table rt_src.orders (
  id bigint generated always as identity primary key,
  customer_id int not null references rt_src.customers(id) on delete cascade,
  total numeric(10,2) default 0 check (total >= 0),
  gross numeric generated always as (total * 1.23) stored,
  tags text[],
  placed date,
  code text
);
create index orders_cust on rt_src.orders (customer_id, placed desc nulls last) where total > 0;
create index orders_tags on rt_src.orders using gin (tags);
create index orders_lower on rt_src.orders (lower(code)) include (placed);
create table rt_src.items (
  order_id bigint references rt_src.orders(id) deferrable initially deferred,
  line int, sku text,
  primary key (order_id, line)
);
create table rt_src.a (id int primary key, b_id int);
create table rt_src.b (id int primary key, a_id int references rt_src.a(id));
alter table rt_src.a add constraint a_b_fk foreign key (b_id) references rt_src.b(id);
create view rt_src.big_orders as select id, total from rt_src.orders where total > 100;
create view rt_src.biggest as select id from rt_src.big_orders where total > 1000;
create materialized view rt_src.totals as select customer_id, sum(total) as s from rt_src.orders group by customer_id;
create function rt_src.order_total(p_id bigint) returns numeric language sql as $$ select total from rt_src.orders where id = p_id $$;
create function rt_src.order_total(p_id bigint, p_tax numeric) returns numeric language sql as $$ select total * p_tax from rt_src.orders where id = p_id $$;
create procedure rt_src.close_order(p_id bigint) language plpgsql as $$ begin update rt_src.orders set total = 0 where id = p_id; end $$;
create function rt_src.touch() returns trigger language plpgsql as $$ begin return new; end $$;
create trigger customers_touch before update on rt_src.customers for each row execute function rt_src.touch();
create table rt_src.events (id int, at date not null) partition by range (at);
create table rt_src.events_2024 partition of rt_src.events for values from ('2024-01-01') to ('2025-01-01');
"#;

#[tokio::test]
async fn postgresql() {
    let Some(mut c) = connect("postgresql", "FERRODB_TCK_POSTGRES").await else {
        return;
    };
    run(c.as_mut(), "postgresql", PG).await;
    let src = introspector(Dialect::Postgres)
        .content(c.as_mut(), "rt_src", None)
        .await
        .unwrap();
    // A few facts besides the round trip.
    let cust = src.table("customers").unwrap();
    assert!(cust.columns[0].serial);
    assert_eq!(cust.uniques.len(), 1);
    assert_eq!(cust.triggers[0].event.as_deref(), Some("BEFORE UPDATE"));
    let orders = src.table("orders").unwrap();
    assert_eq!(orders.columns[0].identity.as_deref(), Some("ALWAYS"));
    assert!(orders.columns[3].generated.is_some());
    assert_eq!(orders.indexes.len(), 3);
    assert_eq!(src.routines.len(), 4);
    assert_eq!(
        src.sequences.len(),
        1,
        "serial / identity sequences are not listed"
    );
    assert_eq!(
        src.table("events_2024")
            .unwrap()
            .partition_of
            .as_ref()
            .unwrap()
            .0,
        "events"
    );
    round_trip(c.as_mut(), Dialect::Postgres, "postgresql", PG).await;
}

const MYSQL: &str = r#"
drop database if exists rt_src;
drop database if exists rt_dst;
create database rt_src;
create database rt_dst;
use rt_src;
create table customers (
  id int unsigned not null auto_increment primary key,
  email varchar(200) not null,
  name varchar(50) default 'anon',
  created datetime default current_timestamp,
  updated timestamp null default current_timestamp on update current_timestamp,
  unique key email_uq (email)
) engine=InnoDB comment='People who buy';
create table orders (
  id bigint not null auto_increment,
  customer_id int unsigned not null,
  total decimal(10,2) not null default 0.00 comment 'Net',
  gross decimal(12,2) generated always as (total * 1.23) stored,
  note text,
  primary key (id),
  key orders_cust (customer_id, total desc),
  constraint orders_fk foreign key (customer_id) references customers (id) on delete cascade,
  constraint total_ck check (total >= 0)
);
create table items (
  order_id bigint not null, line int not null, sku varchar(20),
  primary key (order_id, line),
  key sku_prefix (sku(5)),
  fulltext key sku_ft (sku),
  constraint items_fk foreign key (order_id) references orders(id)
);
create view big_orders as select id, total from orders where total > 100;
create procedure close_order(in p_id bigint, out p_done int)
begin
  update orders set total = 0 where id = p_id;
  set p_done = 1;
end;
create function order_total(p_id bigint) returns decimal(10,2) deterministic reads sql data
  return (select total from orders where id = p_id);
create trigger customers_bu before update on customers for each row set new.name = trim(new.name);
"#;

async fn mysql_family(driver: &str, var: &str, d: Dialect) {
    let Some(mut c) = connect(driver, var).await else {
        return;
    };
    run(c.as_mut(), driver, MYSQL).await;
    let src = introspector(d)
        .content(c.as_mut(), "rt_src", None)
        .await
        .unwrap();
    let orders = src.table("orders").unwrap();
    assert_eq!(orders.primary_key.as_ref().unwrap().columns, vec!["id"]);
    assert_eq!(orders.foreign_keys[0].on_delete.as_deref(), Some("CASCADE"));
    assert_eq!(orders.checks.len(), 1);
    assert!(orders.columns[0].auto_increment);
    assert!(orders.columns[3].generated.is_some());
    assert_eq!(src.routines.len(), 2);
    assert_eq!(src.table("customers").unwrap().triggers.len(), 1);
    round_trip(c.as_mut(), d, driver, MYSQL).await;
}

#[tokio::test]
async fn mysql() {
    mysql_family("mysql", "FERRODB_TCK_MYSQL", Dialect::MySql).await;
}

#[tokio::test]
async fn mariadb() {
    mysql_family("mariadb", "FERRODB_TCK_MARIADB", Dialect::MariaDb).await;
}

/// MongoDB: databases, collections, views, validators, indexes, and the
/// collection script recreating them.
#[tokio::test]
async fn mongodb() {
    use ferrodb_drivers::{Request, sink::collect_request};
    use ferrodb_lang::mql;
    let Some(mut c) = connect("mongodb", "FERRODB_TCK_MONGODB").await else {
        return;
    };
    async fn exec(c: &mut Box<dyn Connection>, text: &str) {
        let cmd = mql::parse(text).unwrap_or_else(|e| panic!("{text}: {e:?}"));
        collect_request(c.as_mut(), Request::Mql(cmd), CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{text}: {e}"));
    }
    for s in [
        "use rt_src",
        "db.dropDatabase()",
        "use rt_dst",
        "db.dropDatabase()",
        "use rt_src",
        "db.createCollection('orders', { validator: { $jsonSchema: { required: ['total'], properties: { total: { bsonType: 'double' } } } } })",
        "db.orders.createIndex({ customer: 1, ts: -1 }, { name: 'cust_ts', unique: true })",
        "db.orders.createIndex({ ts: 1 }, { expireAfterSeconds: 3600 })",
        "db.orders.insertOne({ total: 5.5, customer: 'a', ts: new Date() })",
        "db.createView('big', 'orders', [{ $match: { total: { $gt: 100 } } }])",
        "db.createCollection('metrics', { timeseries: { timeField: 'ts', metaField: 'm' } })",
    ] {
        exec(&mut c, s).await;
    }
    let i = introspector(Dialect::Mongo);
    let dbs = i.containers(c.as_mut()).await.unwrap();
    assert_eq!(
        dbs[0].name, "rt_src",
        "the connection's database comes first"
    );
    let src = i.content(c.as_mut(), "rt_src", None).await.unwrap();
    let names: Vec<&str> = src.collections.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["big", "metrics", "orders"]);
    let orders = &src.collections[2];
    assert!(orders.validator.as_deref().unwrap().contains("$jsonSchema"));
    assert_eq!(orders.indexes.len(), 3);
    assert!(
        orders
            .indexes
            .iter()
            .any(|x| x.unique && x.name == "cust_ts")
    );

    // The database script recreates the same collections and indexes.
    let script = ferrodb_meta::mongo::database_script(&src);
    exec(&mut c, "use rt_dst").await;
    for st in mql::split(&script) {
        exec(&mut c, st.text(&script)).await;
    }
    let dst = i.content(c.as_mut(), "rt_dst", None).await.unwrap();
    let strip = |c: Content| {
        let mut v = serde_json::to_value(&c).unwrap();
        v["refreshed_at"] = 0.into();
        v.to_string().replace("rt_dst", "rt_src")
    };
    assert_eq!(strip(dst), strip(src), "script:\n{script}");
    println!("mongodb: database script round-tripped:\n{script}");
}
