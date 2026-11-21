package dev.ferrodb.jdbchost;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.channels.Channels;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.List;

import com.fasterxml.jackson.databind.node.ObjectNode;

import org.apache.arrow.vector.VectorSchemaRoot;
import org.apache.arrow.vector.ipc.ArrowStreamWriter;
import org.apache.arrow.vector.types.pojo.Field;
import org.apache.arrow.vector.types.pojo.Schema;

/**
 * One result set → one Arrow IPC stream, sent as {@code begin} event, data frames (schema, then
 * one frame per record batch) and {@code end} event. A batch is sent when it has
 * {@code batchRows} rows or has waited for {@link #FLUSH_NANOS}, and only when the client granted
 * a credit for it: a client that stops reading stops the fetch loop, so memory stays bounded.
 */
final class Streamer {
    /** A partially filled batch is sent after this long (slow queries show rows early). */
    private static final long FLUSH_NANOS = 100_000_000L;

    private final Host host;
    private final Call call;
    private final int batchRows;
    private final Buffer buf = new Buffer();
    private VectorSchemaRoot root;
    private ArrowStreamWriter writer;
    private Columns.Col[] cols;
    private int row;
    private long batchStart;

    Streamer(Host host, Call call, int batchRows) {
        this.host = host;
        this.call = call;
        this.batchRows = batchRows;
    }

    /** Exposes the internal array, so a frame is sent without copying. */
    private static final class Buffer extends ByteArrayOutputStream {
        Buffer() {
            super(1 << 16);
        }

        byte[] bytes() {
            return buf;
        }
    }

    boolean started() {
        return root != null;
    }

    /** Streams {@code rs} entirely; {@code true} when the client stopped reading. */
    boolean single(ResultSet rs) throws Exception {
        begin(rs.getMetaData());
        if (rows(rs)) {
            close();
            return true;
        }
        end();
        return false;
    }

    void begin(ResultSetMetaData md) throws SQLException, IOException {
        int n = md.getColumnCount();
        cols = new Columns.Col[n];
        List<Field> fields = new ArrayList<>(n);
        for (int i = 0; i < n; i++) {
            cols[i] = Columns.forColumn(md, i + 1);
            fields.add(cols[i].field);
        }
        root = VectorSchemaRoot.create(new Schema(fields), host.allocator);
        for (int i = 0; i < n; i++) {
            cols[i].bind(root.getVector(i));
        }
        ObjectNode ev = host.json.createObjectNode();
        ev.put("event", "begin");
        host.event(call.id, ev);
        writer = new ArrowStreamWriter(root, null, Channels.newChannel(buf));
        writer.start();
        send();
        root.allocateNew();
        row = 0;
    }

    /** Appends every row of {@code rs}; {@code true} when the client stopped reading. */
    boolean rows(ResultSet rs) throws Exception {
        for (;;) {
            call.checkCancelled();
            if (call.truncated) {
                return true;
            }
            if (!rs.next()) {
                return false;
            }
            if (row == 0) {
                batchStart = System.nanoTime();
            }
            for (int i = 0; i < cols.length; i++) {
                cols[i].read(rs, i + 1, row);
            }
            row++;
            if (row >= batchRows || System.nanoTime() - batchStart > FLUSH_NANOS) {
                if (!flush()) {
                    return true;
                }
            }
        }
    }

    void end() throws Exception {
        if (row > 0 && !flush()) {
            close();
            return;
        }
        close();
        ObjectNode ev = host.json.createObjectNode();
        ev.put("event", "end");
        host.event(call.id, ev);
    }

    private boolean flush() throws Exception {
        root.setRowCount(row);
        if (!call.acquire()) {
            return false;
        }
        writer.writeBatch();
        send();
        root.clear();
        root.allocateNew();
        row = 0;
        return true;
    }

    private void send() {
        if (buf.size() > 0) {
            host.data(call.id, buf.bytes(), buf.size());
            buf.reset();
        }
    }

    /** Releases the vectors of a stream the client stopped reading. */
    void abort() {
        close();
    }

    private void close() {
        if (root != null) {
            root.close();
        }
    }
}
