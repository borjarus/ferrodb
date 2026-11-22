package dev.ferrodb.jdbchost;

import java.io.File;
import java.net.MalformedURLException;
import java.net.URL;
import java.net.URLClassLoader;
import java.sql.Driver;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.ServiceLoader;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Vendor drivers, each in its own class loader. The parent is the platform class loader, so a
 * driver sees the JDK only: never Arrow, Jackson or another vendor's classes (two drivers bundling
 * different versions of one library cannot clash). Loaders are cached per jar list.
 */
final class Drivers {
    private final Map<String, URLClassLoader> loaders = new ConcurrentHashMap<>();

    Driver load(List<String> jars, String className, String url) throws SQLException {
        String key = String.join(File.pathSeparator, jars);
        URLClassLoader loader = loaders.computeIfAbsent(key, k -> {
            List<URL> urls = new ArrayList<>();
            for (String j : jars) {
                try {
                    urls.add(new File(j).toURI().toURL());
                } catch (MalformedURLException e) {
                    throw new IllegalArgumentException("bad jar path " + j, e);
                }
            }
            return new URLClassLoader("jdbc:" + k.hashCode(), urls.toArray(URL[]::new),
                    ClassLoader.getPlatformClassLoader());
        });
        for (String j : jars) {
            if (!new File(j).isFile()) {
                throw new SQLException("driver file not found: " + j, "08001");
            }
        }
        if (className != null && !className.isBlank()) {
            try {
                Class<?> c = Class.forName(className.trim(), true, loader);
                return (Driver) c.getDeclaredConstructor().newInstance();
            } catch (ClassNotFoundException e) {
                throw new SQLException("driver class " + className + " not found in " + jars, "08001", e);
            } catch (ReflectiveOperationException | ClassCastException e) {
                throw new SQLException("cannot instantiate driver " + className + ": " + e, "08001", e);
            }
        }
        // No class given (custom drivers): the first service-registered driver accepting the URL.
        for (Driver d : ServiceLoader.load(Driver.class, loader)) {
            if (d.getClass().getClassLoader() == loader && d.acceptsURL(url)) {
                return d;
            }
        }
        throw new SQLException("no driver in " + jars + " accepts " + url, "08001");
    }
}
