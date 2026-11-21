package dev.ferrodb.jdbchost;

import java.io.BufferedInputStream;
import java.io.BufferedOutputStream;
import java.io.FileDescriptor;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.OutputStream;

/**
 * Entry point of {@code ferrodb-jdbc-host}, the Java side of the FerroDb JDBC bridge (ADR-006).
 *
 * <p>The protocol runs over the process' stdin / stdout (see {@link Wire}). Pipes are private to
 * the parent process (no port, no token, no firewall prompt), and the host exits as soon as its
 * stdin closes, so a crashed IDE never leaves an orphan JVM behind. JDBC drivers that print to
 * {@code System.out} would corrupt the protocol: {@code System.out} is redirected to stderr, which
 * the IDE logs.
 *
 * <p>Run with {@code java --add-opens=java.base/java.nio=ALL-UNNAMED -jar ferrodb-jdbc-host.jar}.
 */
public final class Main {
    private Main() {}

    public static void main(String[] args) throws Exception {
        OutputStream protocol = new FileOutputStream(FileDescriptor.out);
        System.setOut(System.err);
        Wire wire = new Wire(
                new BufferedInputStream(new FileInputStream(FileDescriptor.in), 1 << 16),
                new BufferedOutputStream(protocol, 1 << 16));
        Host host = new Host(wire);
        try {
            host.run();
        } finally {
            host.shutdown();
        }
        System.exit(0);
    }
}
