package dev.ferrodb.jdbchost;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.sql.SQLException;
import java.sql.SQLTimeoutException;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;

import org.apache.arrow.memory.BufferAllocator;
import org.apache.arrow.memory.RootAllocator;

/**
 * Reads requests and dispatches them: every connection has its own thread ({@link Session}), so a
 * long statement on one connection never blocks another; cancellation and credits are handled on
 * the reader thread at once.
 */
final class Host {
    static final int PROTOCOL = 1;
    /** How long a cancelled statement may run on before its session is abandoned. */
    static final long ABANDON_AFTER_MS = 500;

    final Wire wire;
    final ObjectMapper json = new ObjectMapper();
    final BufferAllocator allocator = new RootAllocator();
    final Drivers drivers = new Drivers();
    private final Map<String, Session> sessions = new ConcurrentHashMap<>();
    /** Running (or queued) requests by id: credits and cancellation reach them here. */
    final Map<Long, Call> calls = new ConcurrentHashMap<>();
    private final ExecutorService cancels = Executors.newCachedThreadPool(r -> {
        Thread t = new Thread(r, "jdbc-host-cancel");
        t.setDaemon(true);
        return t;
    });

    Host(Wire wire) {
        this.wire = wire;
    }

    void run() throws IOException {
        for (;;) {
            Wire.Frame f = wire.read();
            if (f == null) {
                return;
            }
            switch (f.kind()) {
                case Wire.CREDIT -> {
                    Call c = calls.get(f.id());
                    if (c != null) {
                        c.credits.release(ByteBuffer.wrap(f.payload()).getInt());
                    }
                }
                case Wire.REQUEST -> {
                    JsonNode req;
                    try {
                        req = json.readTree(f.payload());
                    } catch (IOException e) {
                        error(f.id(), "protocol", "bad request: " + e.getMessage(), null, 0, -1);
                        continue;
                    }
                    if ("shutdown".equals(req.path("method").asText())) {
                        respond(f.id(), json.createObjectNode());
                        return;
                    }
                    dispatch(f.id(), req.path("method").asText(), req.path("params"));
                }
                default -> System.err.println("[jdbc-host] unexpected frame kind " + f.kind());
            }
        }
    }

    private void dispatch(long id, String method, JsonNode p) {
        String conn = p.path("conn").asText("");
        switch (method) {
            case "hello" -> {
                ObjectNode r = json.createObjectNode();
                r.put("protocol", PROTOCOL);
                r.put("java", System.getProperty("java.version"));
                r.put("pid", ProcessHandle.current().pid());
                respond(id, r);
            }
            case "cancel" -> {
                Call c = calls.get(p.path("target").asLong());
                boolean truncate = p.path("truncate").asBoolean(false);
                if (c != null) {
                    // Statement.cancel may block (HyperSQL waits for the busy
                    // session): it runs on its own thread.
                    cancels.submit(() -> c.cancel(truncate));
                    cancels.submit(() -> {
                        if (truncate) {
                            return;
                        }
                        // A driver that ignores the cancel gets its session abandoned.
                        try {
                            Thread.sleep(ABANDON_AFTER_MS);
                        } catch (InterruptedException e) {
                            return;
                        }
                        Session s = c.session;
                        if (!c.done && s != null) {
                            s.abandon(c);
                        }
                    });
                }
                respond(id, json.createObjectNode().put("found", c != null));
            }
            case "connect" -> {
                Session s = new Session(this, conn);
                Session old = sessions.put(conn, s);
                if (old != null) {
                    old.closeAsync();
                }
                submit(s, id, method, p);
            }
            default -> {
                Session s = "close".equals(method) ? sessions.remove(conn) : sessions.get(conn);
                if (s == null) {
                    if ("close".equals(method)) {
                        respond(id, json.createObjectNode());
                    } else {
                        error(id, "connection", "no such connection: " + conn, "08003", 0, -1);
                    }
                    return;
                }
                submit(s, id, method, p);
            }
        }
    }

    private void submit(Session s, long id, String method, JsonNode p) {
        Call call = new Call(id);
        call.credits.release(Math.max(0, p.path("credits").asInt(0)));
        calls.put(id, call);
        s.submit(call, method, p);
    }

    void respond(long id, JsonNode result) {
        ObjectNode r = json.createObjectNode();
        r.set("result", result);
        send(Wire.RESPONSE, id, r);
    }

    void event(long id, ObjectNode ev) {
        send(Wire.EVENT, id, ev);
    }

    void data(long id, byte[] bytes, int len) {
        try {
            wire.write(Wire.DATA, id, bytes, 0, len);
        } catch (IOException e) {
            throw new IllegalStateException("client went away", e);
        }
    }

    void error(long id, String kind, String message, String sqlState, int vendorCode, int position) {
        ObjectNode e = json.createObjectNode();
        e.put("kind", kind);
        e.put("message", message == null ? kind : message);
        if (sqlState != null) {
            e.put("sql_state", sqlState);
        }
        e.put("vendor_code", vendorCode);
        if (position >= 0) {
            e.put("position", position);
        }
        ObjectNode r = json.createObjectNode();
        r.set("error", e);
        send(Wire.RESPONSE, id, r);
    }

    /** Maps a failure of request {@code call} to an error response. */
    void fail(Call call, Throwable t) {
        if (call.cancelled || t instanceof Call.CancelledException) {
            error(call.id, "cancelled", "statement cancelled", "57014", 0, -1);
            return;
        }
        if (t instanceof SQLException e) {
            StringBuilder msg = new StringBuilder(String.valueOf(e.getMessage()));
            SQLException next = e.getNextException();
            for (int i = 0; next != null && i < 5; i++, next = next.getNextException()) {
                if (next.getMessage() != null && !msg.toString().contains(next.getMessage())) {
                    msg.append("\n").append(next.getMessage());
                }
            }
            String state = e.getSQLState();
            String kind = e instanceof SQLTimeoutException ? "timeout"
                    : state != null && state.startsWith("08") ? "connection" : "sql";
            error(call.id, kind, msg.toString(), state, e.getErrorCode(), Errors.position(e));
            return;
        }
        error(call.id, "host", t.toString(), null, 0, -1);
        if (!(t instanceof IllegalArgumentException)) {
            t.printStackTrace(System.err);
        }
    }

    private void send(byte kind, long id, JsonNode body) {
        try {
            wire.write(kind, id, json.writeValueAsBytes(body));
        } catch (IOException e) {
            throw new IllegalStateException("client went away", e);
        }
    }

    void shutdown() {
        for (Session s : sessions.values()) {
            s.closeAsync();
        }
        sessions.clear();
    }
}
