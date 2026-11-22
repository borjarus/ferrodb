package dev.ferrodb.jdbchost;

import java.math.BigDecimal;
import java.math.RoundingMode;
import java.nio.charset.StandardCharsets;
import java.sql.Array;
import java.sql.Clob;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.SQLXML;
import java.sql.Timestamp;
import java.sql.Types;
import java.time.LocalDate;
import java.time.LocalDateTime;
import java.time.LocalTime;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.util.Arrays;
import java.util.Locale;
import java.util.Map;

import org.apache.arrow.vector.BigIntVector;
import org.apache.arrow.vector.BitVector;
import org.apache.arrow.vector.DateDayVector;
import org.apache.arrow.vector.DecimalVector;
import org.apache.arrow.vector.FieldVector;
import org.apache.arrow.vector.Float4Vector;
import org.apache.arrow.vector.Float8Vector;
import org.apache.arrow.vector.IntVector;
import org.apache.arrow.vector.SmallIntVector;
import org.apache.arrow.vector.TimeMicroVector;
import org.apache.arrow.vector.TimeStampMicroTZVector;
import org.apache.arrow.vector.TimeStampMicroVector;
import org.apache.arrow.vector.VarBinaryVector;
import org.apache.arrow.vector.VarCharVector;
import org.apache.arrow.vector.types.DateUnit;
import org.apache.arrow.vector.types.FloatingPointPrecision;
import org.apache.arrow.vector.types.TimeUnit;
import org.apache.arrow.vector.types.pojo.ArrowType;
import org.apache.arrow.vector.types.pojo.Field;
import org.apache.arrow.vector.types.pojo.FieldType;

/**
 * JDBC column → Arrow vector, the same representation the native drivers produce
 * ({@code ferrodb_drivers::columns::ColumnType}): exact decimals, timestamps with and without
 * time zone in microseconds, binary, large objects read whole, and everything without an Arrow
 * equivalent (arrays, structs, XML, intervals, row ids) as text. The database type name is kept in
 * the field metadata under {@code ferrodb.db_type}.
 */
final class Columns {
    private Columns() {}

    static final String DB_TYPE_KEY = "ferrodb.db_type";
    /** Oracle-specific type codes (oracle.jdbc.OracleTypes). */
    private static final int ORACLE_TIMESTAMPTZ = -101;
    private static final int ORACLE_TIMESTAMPLTZ = -102;
    private static final int ORACLE_BINARY_FLOAT = 100;
    private static final int ORACLE_BINARY_DOUBLE = 101;

    /** Reads column {@code i} of the current row into row {@code row} of its vector. */
    abstract static class Col {
        final Field field;
        /** The java.time getter is not supported: use the java.sql one. */
        boolean legacy;

        Col(String name, ArrowType type, String dbType) {
            this.field = new Field(name, new FieldType(true, type, null, Map.of(DB_TYPE_KEY, dbType)),
                    null);
        }

        abstract void bind(FieldVector v);

        abstract void read(ResultSet rs, int i, int row) throws SQLException;
    }

    static Col forColumn(ResultSetMetaData md, int i) throws SQLException {
        String name = md.getColumnLabel(i);
        if (name == null || name.isEmpty()) {
            name = md.getColumnName(i);
        }
        if (name == null) {
            name = "column" + i;
        }
        int type = md.getColumnType(i);
        String typeName = md.getColumnTypeName(i);
        String dbType = typeName == null ? "" : typeName.toLowerCase(Locale.ROOT);
        int precision = safePrecision(md, i);
        int scale = safeScale(md, i);
        boolean signed = safeSigned(md, i);
        switch (type) {
            case Types.BOOLEAN:
                return bool(name, dbType);
            case Types.BIT:
                return precision > 1 ? text(name, dbType) : bool(name, dbType);
            case Types.TINYINT:
            case Types.SMALLINT:
                // TINYINT is unsigned in Sybase / SQL Server: 16 bits hold both.
                return smallint(name, dbType);
            case Types.INTEGER:
                return signed ? integer(name, dbType) : bigint(name, dbType);
            case Types.BIGINT:
                return signed ? bigint(name, dbType) : decimal(name, dbType, 20, 0);
            case Types.REAL:
            case ORACLE_BINARY_FLOAT:
                return real(name, dbType);
            case Types.FLOAT:
            case Types.DOUBLE:
            case ORACLE_BINARY_DOUBLE:
                return dbl(name, dbType);
            case Types.NUMERIC:
            case Types.DECIMAL:
                if (precision >= 1 && precision <= 38 && scale >= 0 && scale <= precision) {
                    return decimal(name, dbType, precision, scale);
                }
                // Oracle NUMBER without precision (p = 0, s = -127), DECFLOAT, huge precisions:
                // exact text rather than a lossy double.
                return decimalText(name, dbType);
            case Types.CHAR:
            case Types.VARCHAR:
            case Types.LONGVARCHAR:
            case Types.NCHAR:
            case Types.NVARCHAR:
            case Types.LONGNVARCHAR:
            case Types.CLOB:
            case Types.NCLOB:
                return text(name, dbType);
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB:
                return binary(name, dbType);
            case Types.DATE:
                return date(name, dbType);
            case Types.TIME:
                return time(name, dbType);
            case Types.TIMESTAMP:
                return timestamp(name, dbType);
            case Types.TIMESTAMP_WITH_TIMEZONE:
            case ORACLE_TIMESTAMPTZ:
                return timestampTz(name, dbType, false);
            case ORACLE_TIMESTAMPLTZ:
                return timestampTz(name, dbType, true);
            default:
                // ARRAY, STRUCT, SQLXML, OTHER, ROWID, intervals, TIME WITH TIME ZONE…
                return text(name, dbType);
        }
    }

    private static int safePrecision(ResultSetMetaData md, int i) {
        try {
            return md.getPrecision(i);
        } catch (SQLException | RuntimeException e) {
            return 0;
        }
    }

    private static int safeScale(ResultSetMetaData md, int i) {
        try {
            return md.getScale(i);
        } catch (SQLException | RuntimeException e) {
            return 0;
        }
    }

    private static boolean safeSigned(ResultSetMetaData md, int i) {
        try {
            return md.isSigned(i);
        } catch (SQLException | RuntimeException e) {
            return true;
        }
    }

    private static Col bool(String name, String db) {
        return new Col(name, ArrowType.Bool.INSTANCE, db) {
            BitVector v;

            void bind(FieldVector f) {
                v = (BitVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                boolean x = rs.getBoolean(i);
                if (rs.wasNull()) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, x ? 1 : 0);
                }
            }
        };
    }

    private static Col smallint(String name, String db) {
        return new Col(name, new ArrowType.Int(16, true), db) {
            SmallIntVector v;

            void bind(FieldVector f) {
                v = (SmallIntVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                short x = rs.getShort(i);
                if (rs.wasNull()) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, x);
                }
            }
        };
    }

    private static Col integer(String name, String db) {
        return new Col(name, new ArrowType.Int(32, true), db) {
            IntVector v;

            void bind(FieldVector f) {
                v = (IntVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                int x = rs.getInt(i);
                if (rs.wasNull()) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, x);
                }
            }
        };
    }

    private static Col bigint(String name, String db) {
        return new Col(name, new ArrowType.Int(64, true), db) {
            BigIntVector v;

            void bind(FieldVector f) {
                v = (BigIntVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                long x = rs.getLong(i);
                if (rs.wasNull()) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, x);
                }
            }
        };
    }

    private static Col real(String name, String db) {
        return new Col(name, new ArrowType.FloatingPoint(FloatingPointPrecision.SINGLE), db) {
            Float4Vector v;

            void bind(FieldVector f) {
                v = (Float4Vector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                float x = rs.getFloat(i);
                if (rs.wasNull()) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, x);
                }
            }
        };
    }

    private static Col dbl(String name, String db) {
        return new Col(name, new ArrowType.FloatingPoint(FloatingPointPrecision.DOUBLE), db) {
            Float8Vector v;

            void bind(FieldVector f) {
                v = (Float8Vector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                double x = rs.getDouble(i);
                if (rs.wasNull()) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, x);
                }
            }
        };
    }

    private static Col decimal(String name, String db, int precision, int scale) {
        return new Col(name, new ArrowType.Decimal(precision, scale, 128), db) {
            DecimalVector v;

            void bind(FieldVector f) {
                v = (DecimalVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                BigDecimal x = rs.getBigDecimal(i);
                if (x == null) {
                    v.setNull(row);
                    return;
                }
                try {
                    v.setSafe(row, x.setScale(scale, RoundingMode.HALF_UP));
                } catch (UnsupportedOperationException | ArithmeticException e) {
                    // More digits than the declared precision (computed columns): no value is
                    // better than a wrong one.
                    v.setNull(row);
                }
            }
        };
    }

    private static Col decimalText(String name, String db) {
        return new Col(name, ArrowType.Utf8.INSTANCE, db) {
            VarCharVector v;

            void bind(FieldVector f) {
                v = (VarCharVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                BigDecimal x = rs.getBigDecimal(i);
                if (x == null) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, x.toPlainString().getBytes(StandardCharsets.UTF_8));
                }
            }
        };
    }

    private static Col text(String name, String db) {
        return new Col(name, ArrowType.Utf8.INSTANCE, db) {
            VarCharVector v;

            void bind(FieldVector f) {
                v = (VarCharVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                String s = legacy ? fallback(rs, i) : null;
                if (!legacy) {
                    try {
                        s = rs.getString(i);
                    } catch (SQLException e) {
                        legacy = true;
                        s = fallback(rs, i);
                    }
                }
                if (s == null) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, s.getBytes(StandardCharsets.UTF_8));
                }
            }
        };
    }

    /** Text of values {@code getString} refuses (arrays, XML, CLOBs of some drivers). */
    private static String fallback(ResultSet rs, int i) throws SQLException {
        Object o = rs.getObject(i);
        if (o == null) {
            return null;
        }
        if (o instanceof Array a) {
            Object arr = a.getArray();
            return arr instanceof Object[] objs ? Arrays.deepToString(objs) : String.valueOf(arr);
        }
        if (o instanceof Clob c) {
            return c.getSubString(1, (int) Math.min(Integer.MAX_VALUE, c.length()));
        }
        if (o instanceof SQLXML x) {
            return x.getString();
        }
        if (o instanceof byte[] b) {
            return new String(b, StandardCharsets.UTF_8);
        }
        return o.toString();
    }

    private static Col binary(String name, String db) {
        return new Col(name, ArrowType.Binary.INSTANCE, db) {
            VarBinaryVector v;

            void bind(FieldVector f) {
                v = (VarBinaryVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                byte[] b = rs.getBytes(i);
                if (b == null) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, b);
                }
            }
        };
    }

    private static Col date(String name, String db) {
        return new Col(name, new ArrowType.Date(DateUnit.DAY), db) {
            DateDayVector v;

            void bind(FieldVector f) {
                v = (DateDayVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                LocalDate d = null;
                if (!legacy) {
                    try {
                        d = rs.getObject(i, LocalDate.class);
                    } catch (SQLException | AbstractMethodError | UnsupportedOperationException
                            | ClassCastException e) {
                        legacy = true;
                    }
                }
                if (legacy) {
                    java.sql.Date x = rs.getDate(i);
                    d = x == null ? null : x.toLocalDate();
                }
                if (d == null) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, (int) d.toEpochDay());
                }
            }
        };
    }

    private static Col time(String name, String db) {
        return new Col(name, new ArrowType.Time(TimeUnit.MICROSECOND, 64), db) {
            TimeMicroVector v;

            void bind(FieldVector f) {
                v = (TimeMicroVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                LocalTime t = null;
                if (!legacy) {
                    try {
                        t = rs.getObject(i, LocalTime.class);
                    } catch (SQLException | AbstractMethodError | UnsupportedOperationException
                            | ClassCastException e) {
                        legacy = true;
                    }
                }
                if (legacy) {
                    java.sql.Time x = rs.getTime(i);
                    t = x == null ? null : x.toLocalTime();
                }
                if (t == null) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, t.toNanoOfDay() / 1000);
                }
            }
        };
    }

    private static long micros(LocalDateTime t) {
        return t.toEpochSecond(ZoneOffset.UTC) * 1_000_000L + t.getNano() / 1000;
    }

    private static Col timestamp(String name, String db) {
        return new Col(name, new ArrowType.Timestamp(TimeUnit.MICROSECOND, null), db) {
            TimeStampMicroVector v;

            void bind(FieldVector f) {
                v = (TimeStampMicroVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                LocalDateTime t = null;
                if (!legacy) {
                    try {
                        t = rs.getObject(i, LocalDateTime.class);
                    } catch (SQLException | AbstractMethodError | UnsupportedOperationException
                            | ClassCastException e) {
                        legacy = true;
                    }
                }
                if (legacy) {
                    Timestamp x = rs.getTimestamp(i);
                    t = x == null ? null : x.toLocalDateTime();
                }
                if (t == null) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, micros(t));
                }
            }
        };
    }

    /** Instants, shown in UTC. {@code local}: Oracle TIMESTAMP WITH LOCAL TIME ZONE. */
    private static Col timestampTz(String name, String db, boolean local) {
        return new Col(name, new ArrowType.Timestamp(TimeUnit.MICROSECOND, "UTC"), db) {
            TimeStampMicroTZVector v;

            void bind(FieldVector f) {
                v = (TimeStampMicroTZVector) f;
            }

            void read(ResultSet rs, int i, int row) throws SQLException {
                Long us = null;
                if (!legacy && !local) {
                    try {
                        OffsetDateTime t = rs.getObject(i, OffsetDateTime.class);
                        us = t == null ? null
                                : t.toEpochSecond() * 1_000_000L + t.getNano() / 1000;
                        if (t == null && !rs.wasNull()) {
                            legacy = true;
                        }
                    } catch (SQLException | AbstractMethodError | UnsupportedOperationException
                            | ClassCastException e) {
                        legacy = true;
                    }
                }
                if (legacy || local) {
                    Timestamp x = rs.getTimestamp(i);
                    us = x == null ? null
                            : x.toInstant().getEpochSecond() * 1_000_000L + x.getNanos() / 1000;
                }
                if (us == null) {
                    v.setNull(row);
                } else {
                    v.setSafe(row, us);
                }
            }
        };
    }
}
