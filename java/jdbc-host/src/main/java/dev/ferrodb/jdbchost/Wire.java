package dev.ferrodb.jdbchost;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * Framing of the bridge protocol on the child's stdin / stdout.
 *
 * <p>Every frame is {@code [u8 kind][u64 id][u32 length][payload]}, big endian. Requests,
 * responses and events carry UTF-8 JSON; data frames carry Arrow IPC stream bytes; credit frames
 * carry an {@code i32} number of record batches the client is ready to receive (backpressure).
 * Frames of different requests interleave freely: the id names the request they belong to.
 */
final class Wire {
    /** Client → host: a JSON-RPC request {@code {"method": …, "params": {…}}}. */
    static final byte REQUEST = 1;
    /** Host → client: the final JSON answer of a request ({@code result} or {@code error}). */
    static final byte RESPONSE = 2;
    /** Host → client: JSON progress of a running request (result set begin / end, notices). */
    static final byte EVENT = 3;
    /** Host → client: Arrow IPC stream bytes of the current result set. */
    static final byte DATA = 4;
    /** Client → host: more record batches may be sent for this request. */
    static final byte CREDIT = 5;

    record Frame(byte kind, long id, byte[] payload) {}

    private final DataInputStream in;
    private final DataOutputStream out;

    Wire(InputStream in, OutputStream out) {
        this.in = new DataInputStream(in);
        this.out = new DataOutputStream(out);
    }

    /** The next frame, {@code null} at end of input (the client went away). */
    Frame read() throws IOException {
        int kind = in.read();
        if (kind < 0) {
            return null;
        }
        try {
            long id = in.readLong();
            int len = in.readInt();
            if (len < 0 || len > (256 << 20)) {
                throw new IOException("bad frame length " + len);
            }
            byte[] payload = new byte[len];
            in.readFully(payload);
            return new Frame((byte) kind, id, payload);
        } catch (EOFException e) {
            return null;
        }
    }

    /** Writes one frame atomically (frames of concurrent requests never interleave bytes). */
    synchronized void write(byte kind, long id, byte[] payload, int off, int len) throws IOException {
        out.writeByte(kind);
        out.writeLong(id);
        out.writeInt(len);
        out.write(payload, off, len);
        out.flush();
    }

    void write(byte kind, long id, byte[] payload) throws IOException {
        write(kind, id, payload, 0, payload.length);
    }
}
