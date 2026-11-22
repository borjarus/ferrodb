package dev.ferrodb.jdbchost;

import java.lang.reflect.Method;

/** Error details that only vendor classes know. Message-based positions are read by the client. */
final class Errors {
    private Errors() {}

    /** 0-based character offset of the error in the statement, or -1. */
    static int position(Throwable e) {
        for (Throwable t = e; t != null; t = t.getCause()) {
            // Oracle: oracle.jdbc.OracleDatabaseException#getErrorPosition (ojdbc8+).
            if (t.getClass().getName().equals("oracle.jdbc.OracleDatabaseException")) {
                try {
                    Method m = t.getClass().getMethod("getErrorPosition");
                    Object v = m.invoke(t);
                    if (v instanceof Integer i && i >= 0) {
                        return i;
                    }
                } catch (ReflectiveOperationException | RuntimeException ignored) {
                    return -1;
                }
            }
            if (t.getCause() == t) {
                break;
            }
        }
        return -1;
    }
}
