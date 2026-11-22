package dev.ferrodb.jdbchost;

import java.sql.SQLFeatureNotSupportedException;
import java.sql.Statement;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;

/** One running request: its credits (backpressure) and its cancellation state. */
final class Call {
    final long id;
    /** Record batches the client is ready to receive. */
    final Semaphore credits = new Semaphore(0);
    /** The client cancelled the request: stop and answer with a cancellation error. */
    volatile boolean cancelled;
    /** The client stopped reading: stop quietly and report the result as truncated. */
    volatile boolean truncated;
    /** The statement being executed, cancelled from another thread. */
    volatile Statement statement;
    /** The session running the call (set when it is queued). */
    volatile Session session;
    /** The call's task has finished. */
    volatile boolean done;
    private final java.util.concurrent.atomic.AtomicBoolean answered =
            new java.util.concurrent.atomic.AtomicBoolean();

    Call(long id) {
        this.id = id;
    }

    /** Runs on a cancel thread, never on the reader or the session thread. */
    void cancel(boolean truncate) {
        if (truncate) {
            truncated = true;
        } else {
            cancelled = true;
        }
        credits.release(1 << 20);
        Statement s = statement;
        if (s != null && !truncate) {
            try {
                s.cancel();
            } catch (SQLFeatureNotSupportedException e) {
                // Derby: no statement cancel; the fetch loop checks the flag between rows.
            } catch (Throwable t) {
                System.err.println("[jdbc-host] cancel failed: " + t);
            }
        }
    }

    /** {@code true} once: the call may be answered (normally or as abandoned). */
    boolean answer() {
        return answered.compareAndSet(false, true);
    }

    boolean stopped() {
        return cancelled || truncated;
    }

    void checkCancelled() throws CancelledException {
        if (cancelled) {
            throw new CancelledException();
        }
    }

    /** Waits for one credit; {@code false} when the request was stopped meanwhile. */
    boolean acquire() throws InterruptedException, CancelledException {
        while (!credits.tryAcquire(50, TimeUnit.MILLISECONDS)) {
            checkCancelled();
            if (truncated) {
                return false;
            }
        }
        checkCancelled();
        return !truncated;
    }

    static final class CancelledException extends Exception {
        CancelledException() {
            super("cancelled", null, false, false);
        }
    }
}
