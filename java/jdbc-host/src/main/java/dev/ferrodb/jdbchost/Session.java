package dev.ferrodb.jdbchost;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.Driver;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.SQLFeatureNotSupportedException;
import java.sql.SQLWarning;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.Properties;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.RejectedExecutionException;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.node.ObjectNode;

/** One JDBC connection and the thread that runs its requests, in order. */
final class Session {
    private final Host host;
    private final String id;
    /** Replaced when a statement ignoring cancellation is abandoned. */
    private volatile ExecutorService exec;
    private volatile Connection conn;
    /** Parameters of `connect`, to reconnect after abandoning a statement. */
    private volatile JsonNode connectParams;

    Session(Host host, String id) {
        this.host = host;
        this.id = id;
        this.exec = newExecutor();
    }

    private ExecutorService newExecutor() {
        return Executors.newSingleThreadExecutor(r -> {
            Thread t = new Thread(r, "jdbc-conn-" + id);
            t.setDaemon(true);
            return t;
        });
    }

    void submit(Call call, String method, JsonNode p) {
        call.session = this;
        ExecutorService ex = exec;
        try {
            ex.submit(() -> {
                try {
                    JsonNode r = handle(call, method, p);
                    if (call.answer()) {
                        host.respond(call.id, r);
                    }
                } catch (Throwable t) {
                    if (call.answer()) {
                        host.fail(call, t);
                    }
                } finally {
                    call.done = true;
                    host.calls.remove(call.id);
                    if ("close".equals(method)) {
                        ex.shutdown();
                    }
                }
            });
        } catch (RejectedExecutionException e) {
            host.calls.remove(call.id);
            host.error(call.id, "connection", "connection " + id + " is closed", "08003", 0, -1);
        }
    }

    /**
     * The driver ignored the cancel of {@code call} (HyperSQL and Derby in
     * embedded mode, some old drivers): the call is answered as cancelled at
     * once, its thread and JDBC connection are abandoned (closed when the
     * statement ends) and the session continues on a fresh connection, so the
     * user gets the console back. Session state (transaction, schema) is lost.
     */
    synchronized void abandon(Call call) {
        if (call.done || !call.answer()) {
            return;
        }
        System.err.println("[jdbc-host] " + id + ": cancel ignored by the driver, reconnecting");
        ExecutorService old = exec;
        Connection oldConn = conn;
        JsonNode params = connectParams;
        // The reconnect is queued before the new executor takes requests.
        ExecutorService fresh = newExecutor();
        conn = null;
        fresh.submit(() -> {
            try {
                if (params != null) {
                    connect(params);
                }
            } catch (Throwable t) {
                System.err.println("[jdbc-host] " + id + ": reconnect failed: " + t);
            }
        });
        exec = fresh;
        // Answered last: the client's next request must find the new executor.
        host.error(call.id, "cancelled",
                "statement cancelled; the driver ignored the cancel, the session was reconnected",
                "57014", 0, -1);
        old.submit(() -> {
            try {
                if (oldConn != null) {
                    oldConn.close();
                }
            } catch (Throwable ignored) {
                // abandoned anyway
            }
        });
        old.shutdown();
    }

    void closeAsync() {
        try {
            exec.submit(this::closeQuietly);
        } catch (RejectedExecutionException ignored) {
            // already closed
        }
        exec.shutdown();
    }

    private Connection conn() throws SQLException {
        if (conn == null) {
            throw new SQLException("not connected", "08003");
        }
        return conn;
    }

    private ObjectNode ok() {
        return host.json.createObjectNode();
    }

    private JsonNode handle(Call call, String method, JsonNode p) throws Exception {
        switch (method) {
            case "connect":
                return connect(p);
            case "execute":
                return execute(call, p);
            case "meta":
                return meta(call, p);
            case "info":
                return info();
            case "set_read_only":
                conn().setReadOnly(p.path("value").asBoolean());
                return ok();
            case "set_autocommit":
                conn().setAutoCommit(p.path("value").asBoolean());
                return ok();
            case "commit":
                if (!conn().getAutoCommit()) {
                    conn().commit();
                }
                return ok();
            case "rollback":
                if (!conn().getAutoCommit()) {
                    conn().rollback();
                }
                return ok();
            case "ping":
                ping(p.path("sql").asText(""));
                return ok();
            case "set_schema":
                setSchema(p);
                return ok();
            case "current_schema":
                return ok().put("schema", currentSchema(p));
            case "close":
                closeQuietly();
                return ok();
            default:
                throw new IllegalArgumentException("unknown method " + method);
        }
    }

    private ObjectNode connect(JsonNode p) throws SQLException {
        connectParams = p;
        List<String> jars = new ArrayList<>();
        p.path("jars").forEach(j -> jars.add(j.asText()));
        String url = p.path("url").asText();
        Driver driver = host.drivers.load(jars, p.path("driver_class").asText(""), url);
        Properties props = new Properties();
        Iterator<Map.Entry<String, JsonNode>> it = p.path("properties").fields();
        while (it.hasNext()) {
            Map.Entry<String, JsonNode> e = it.next();
            props.setProperty(e.getKey(), e.getValue().asText());
        }
        Connection c = driver.connect(url, props);
        if (c == null) {
            throw new SQLException("the driver does not accept the URL " + url, "08001");
        }
        conn = c;
        if (p.path("read_only").asBoolean(false)) {
            try {
                c.setReadOnly(true);
            } catch (SQLException e) {
                System.err.println("[jdbc-host] setReadOnly: " + e.getMessage());
            }
        }
        for (JsonNode s : p.path("init")) {
            try (Statement st = c.createStatement()) {
                st.execute(s.asText());
            }
        }
        return info();
    }

    private ObjectNode info() throws SQLException {
        DatabaseMetaData md = conn().getMetaData();
        ObjectNode r = ok();
        r.put("product", md.getDatabaseProductName());
        r.put("version", md.getDatabaseProductVersion());
        safe(() -> r.put("major", md.getDatabaseMajorVersion()));
        safe(() -> r.put("minor", md.getDatabaseMinorVersion()));
        r.put("driver", md.getDriverName() + " " + md.getDriverVersion());
        safe(() -> r.put("user", md.getUserName()));
        safe(() -> r.put("url", md.getURL()));
        safe(() -> r.put("quote", md.getIdentifierQuoteString()));
        safe(() -> r.put("schema_term", md.getSchemaTerm()));
        safe(() -> r.put("catalog_term", md.getCatalogTerm()));
        safe(() -> r.put("transactions", md.supportsTransactions()));
        safe(() -> r.put("keywords", md.getSQLKeywords()));
        safe(() -> r.put("catalog", conn.getCatalog()));
        safe(() -> r.put("schema", conn.getSchema()));
        safe(() -> r.put("read_only", conn.isReadOnly()));
        return r;
    }

    private interface Unsafe {
        void run() throws Exception;
    }

    /** Optional metadata: old drivers throw or lack the method (AbstractMethodError). */
    private static void safe(Unsafe u) {
        try {
            u.run();
        } catch (Throwable ignored) {
            // not supported by this driver
        }
    }

    private void ping(String sql) throws SQLException {
        if (!sql.isEmpty()) {
            try (Statement st = conn().createStatement()) {
                st.execute(sql);
            }
            return;
        }
        boolean valid;
        try {
            valid = conn().isValid(5);
        } catch (SQLFeatureNotSupportedException | AbstractMethodError e) {
            valid = !conn().isClosed();
        }
        if (!valid) {
            throw new SQLException("the connection is no longer valid", "08003");
        }
    }

    private void setSchema(JsonNode p) throws SQLException {
        String sql = p.path("sql").asText("");
        String schema = p.path("schema").asText();
        if (!sql.isEmpty()) {
            try (Statement st = conn().createStatement()) {
                st.execute(sql);
            }
        } else if (p.path("catalog").asBoolean(false)) {
            conn().setCatalog(schema);
        } else {
            conn().setSchema(schema);
        }
    }

    private String currentSchema(JsonNode p) throws SQLException {
        String sql = p.path("sql").asText("");
        if (!sql.isEmpty()) {
            try (Statement st = conn().createStatement(); ResultSet rs = st.executeQuery(sql)) {
                return rs.next() ? rs.getString(1) : null;
            }
        }
        if (p.path("catalog").asBoolean(false)) {
            return conn().getCatalog();
        }
        try {
            return conn().getSchema();
        } catch (AbstractMethodError | SQLFeatureNotSupportedException e) {
            return null;
        }
    }

    private ObjectNode execute(Call call, JsonNode p) throws Exception {
        String sql = p.path("sql").asText();
        int batchRows = Math.max(1, p.path("batch_rows").asInt(1000));
        Statement st = conn().createStatement();
        call.statement = st;
        long affected = -1;
        int results = 0;
        boolean truncated = false;
        try {
            call.checkCancelled();
            try {
                st.setFetchSize(batchRows);
            } catch (SQLException | RuntimeException ignored) {
                // a hint only
            }
            int timeout = p.path("timeout_secs").asInt(0);
            if (timeout > 0) {
                st.setQueryTimeout(timeout);
            }
            boolean isResult = st.execute(sql);
            warnings(call, st.getWarnings());
            for (;;) {
                call.checkCancelled();
                if (isResult) {
                    try (ResultSet rs = st.getResultSet()) {
                        results++;
                        Streamer s = new Streamer(host, call, batchRows);
                        truncated = s.single(rs);
                    }
                    if (truncated) {
                        break;
                    }
                } else {
                    long n = st.getUpdateCount();
                    if (n < 0) {
                        break;
                    }
                    affected = (affected < 0 ? 0 : affected) + n;
                }
                try {
                    isResult = st.getMoreResults();
                } catch (SQLFeatureNotSupportedException | AbstractMethodError e) {
                    break;
                }
            }
        } finally {
            call.statement = null;
            try {
                st.close();
            } catch (SQLException ignored) {
                // closing after a cancel may fail; the statement is gone either way
            }
        }
        ObjectNode r = ok();
        if (affected >= 0) {
            r.put("affected", affected);
        } else {
            r.putNull("affected");
        }
        r.put("results", results);
        r.put("truncated", truncated);
        return r;
    }

    private void warnings(Call call, SQLWarning w) {
        for (int i = 0; w != null && i < 100; i++, w = w.getNextWarning()) {
            ObjectNode ev = host.json.createObjectNode();
            ev.put("event", "notice");
            ev.put("message", String.valueOf(w.getMessage()));
            if (w.getSQLState() != null) {
                ev.put("sql_state", w.getSQLState());
            }
            host.event(call.id, ev);
        }
    }

    private static String arg(JsonNode args, String key) {
        JsonNode v = args.get(key);
        return v == null || v.isNull() ? null : v.asText();
    }

    /** A {@link DatabaseMetaData} call for one table. */
    private interface PerTable {
        ResultSet call(DatabaseMetaData md, String catalog, String schema, String table)
                throws SQLException;
    }

    private ObjectNode meta(Call call, JsonNode p) throws Exception {
        DatabaseMetaData md = conn().getMetaData();
        String method = p.path("method").asText();
        JsonNode a = p.path("args");
        String catalog = arg(a, "catalog");
        String schema = arg(a, "schema");
        String table = arg(a, "table");
        int batchRows = Math.max(1, p.path("batch_rows").asInt(1000));
        PerTable perTable = switch (method) {
            case "columns" -> (m, c, s, t) -> m.getColumns(c, s, t, arg(a, "column"));
            case "primary_keys" -> DatabaseMetaData::getPrimaryKeys;
            case "imported_keys" -> DatabaseMetaData::getImportedKeys;
            case "exported_keys" -> DatabaseMetaData::getExportedKeys;
            case "index_info" -> (m, c, s, t) -> m.getIndexInfo(c, s, t,
                    a.path("unique").asBoolean(false), true);
            default -> null;
        };
        Streamer out = new Streamer(host, call, batchRows);
        boolean truncated;
        if (perTable != null && a.path("each_table").asBoolean(false)) {
            // One stream for every table of the schema: drivers that need a table name (Oracle,
            // Db2) answer in one round trip instead of one per table.
            List<String[]> tables = new ArrayList<>();
            try (ResultSet rs = md.getTables(catalog, schema, table == null ? "%" : table,
                    types(a))) {
                while (rs.next()) {
                    tables.add(new String[] {rs.getString(1), rs.getString(2), rs.getString(3)});
                }
            }
            truncated = false;
            for (String[] t : tables) {
                call.checkCancelled();
                try (ResultSet rs = perTable.call(md, t[0], t[1], t[2])) {
                    if (!out.started()) {
                        out.begin(rs.getMetaData());
                    }
                    if (out.rows(rs)) {
                        truncated = true;
                        out.abort();
                        break;
                    }
                } catch (SQLException e) {
                    // A table dropped meanwhile or without privileges: skip it.
                    System.err.println("[jdbc-host] " + method + " " + t[2] + ": " + e.getMessage());
                }
            }
            if (out.started() && !truncated) {
                out.end();
            }
        } else {
            ResultSet rs = switch (method) {
                case "catalogs" -> md.getCatalogs();
                case "schemas" -> schemas(md, catalog, schema);
                case "tables" -> md.getTables(catalog, schema, table == null ? "%" : table, types(a));
                case "procedures" -> md.getProcedures(catalog, schema, def(arg(a, "procedure")));
                case "procedure_columns" -> md.getProcedureColumns(catalog, schema,
                        def(arg(a, "procedure")), arg(a, "column"));
                case "functions" -> md.getFunctions(catalog, schema, def(arg(a, "function")));
                case "function_columns" -> md.getFunctionColumns(catalog, schema,
                        def(arg(a, "function")), arg(a, "column"));
                case "table_types" -> md.getTableTypes();
                case "type_info" -> md.getTypeInfo();
                default -> {
                    if (perTable == null) {
                        throw new IllegalArgumentException("unknown metadata call " + method);
                    }
                    yield perTable.call(md, catalog, schema, table);
                }
            };
            try (rs) {
                truncated = out.single(rs);
            }
        }
        ObjectNode r = ok();
        r.putNull("affected");
        r.put("results", 1);
        r.put("truncated", truncated);
        return r;
    }

    private static String def(String pattern) {
        return pattern == null ? "%" : pattern;
    }

    private static ResultSet schemas(DatabaseMetaData md, String catalog, String pattern)
            throws SQLException {
        try {
            return md.getSchemas(catalog, pattern);
        } catch (SQLFeatureNotSupportedException | AbstractMethodError e) {
            return md.getSchemas();
        }
    }

    private static String[] types(JsonNode a) {
        String t = arg(a, "types");
        return t == null || t.isBlank() ? null : t.split(",");
    }

    private void closeQuietly() {
        if (conn != null) {
            try {
                if (!conn.getAutoCommit()) {
                    conn.rollback();
                }
            } catch (Throwable ignored) {
                // closing anyway
            }
            try {
                conn.close();
            } catch (Throwable ignored) {
                // nothing to do
            }
            conn = null;
        }
    }
}
