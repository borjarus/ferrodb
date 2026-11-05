package dev.ferrodb.spike;

import java.io.BufferedOutputStream;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.Socket;
import java.nio.channels.Channels;
import java.nio.charset.StandardCharsets;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.ResultSet;
import java.sql.Statement;
import java.util.Calendar;
import java.util.TimeZone;

import org.apache.arrow.adapter.jdbc.ArrowVectorIterator;
import org.apache.arrow.adapter.jdbc.JdbcToArrow;
import org.apache.arrow.adapter.jdbc.JdbcToArrowConfig;
import org.apache.arrow.adapter.jdbc.JdbcToArrowConfigBuilder;
import org.apache.arrow.memory.BufferAllocator;
import org.apache.arrow.memory.RootAllocator;
import org.apache.arrow.vector.VectorSchemaRoot;
import org.apache.arrow.vector.ipc.ArrowStreamWriter;

/**
 * JDBC bridge spike: connects back to the Rust process over a local socket,
 * authenticates with a one-time token, runs a query in H2 and streams the
 * result as Arrow IPC (stream format).
 *
 * <p>Usage: {@code java --add-opens=java.base/java.nio=ALL-UNNAMED -jar jdbc-host-spike-all.jar
 * <port> <rows>}; the token is read from the {@code FERRODB_BRIDGE_TOKEN} environment variable
 * (never from arguments, which other processes can read).
 */
public final class JdbcHost {
    public static void main(String[] args) throws Exception {
        int port = Integer.parseInt(args[0]);
        long rows = Long.parseLong(args[1]);
        String token = System.getenv("FERRODB_BRIDGE_TOKEN");
        if (token == null) {
            throw new IllegalStateException("FERRODB_BRIDGE_TOKEN not set");
        }

        try (Socket socket = new Socket(InetAddress.getLoopbackAddress(), port);
             OutputStream out = new BufferedOutputStream(socket.getOutputStream(), 1 << 20);
             BufferAllocator allocator = new RootAllocator();
             Connection conn = DriverManager.getConnection("jdbc:h2:mem:spike")) {
            out.write((token + "\n").getBytes(StandardCharsets.UTF_8));

            String sql = "SELECT X AS ID, CONCAT('name ', X) AS NAME, X * 0.5 AS AMOUNT, "
                + "MOD(X, 2) = 0 AS FLAG, CURRENT_TIMESTAMP AS TS FROM SYSTEM_RANGE(1, " + rows + ")";
            try (Statement st = conn.createStatement()) {
                st.setFetchSize(4096);
                try (ResultSet rs = st.executeQuery(sql)) {
                    JdbcToArrowConfig config = new JdbcToArrowConfigBuilder(
                            allocator, Calendar.getInstance(TimeZone.getTimeZone("UTC")))
                        .setTargetBatchSize(4096)
                        .setReuseVectorSchemaRoot(true)
                        .build();
                    try (ArrowVectorIterator it = JdbcToArrow.sqlToArrowVectorIterator(rs, config)) {
                        ArrowStreamWriter writer = null;
                        VectorSchemaRoot root = null;
                        while (it.hasNext()) {
                            root = it.next();
                            if (writer == null) {
                                writer = new ArrowStreamWriter(root, null, Channels.newChannel(out));
                                writer.start();
                            }
                            writer.writeBatch();
                        }
                        if (writer != null) {
                            writer.end();
                            writer.close();
                        }
                        if (root != null) {
                            root.close();
                        }
                    }
                }
            }
            out.flush();
        }
    }
}
