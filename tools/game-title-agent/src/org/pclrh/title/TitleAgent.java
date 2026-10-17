package org.pclrh.title;

import java.io.File;
import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.nio.ByteBuffer;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.security.ProtectionDomain;
import java.util.jar.JarFile;

/** Changes only title arguments, on the game's own rendering thread. */
public final class TitleAgent implements ClassFileTransformer {
    private boolean reported;

    public static void premain(String encoded, Instrumentation instrumentation) {
        try {
            String title = decode(encoded);
            // Bootstrap visibility avoids assuming Minecraft's class loader
            // delegates to the system loader. Keep the JarFile open for JVM use.
            File source = new File(TitleAgent.class.getProtectionDomain()
                    .getCodeSource().getLocation().toURI());
            instrumentation.appendToBootstrapClassLoaderSearch(new JarFile(source));
            Class.forName("org.pclrh.title.TitleText", true, null)
                    .getMethod("configure", String.class).invoke(null, title);
            instrumentation.addTransformer(new TitleAgent(), false);
        } catch (Exception error) {
            // A cosmetic option must never abort JVM startup. Do not echo the
            // title or exception text, which can include application data.
            System.err.println("[PCL-RH title] Window title agent could not initialize.");
        } catch (LinkageError error) {
            System.err.println("[PCL-RH title] Window title agent is unavailable in this JVM.");
        }
    }

    private static String decode(String encoded) throws Exception {
        if (encoded == null || encoded.length() == 0 || encoded.length() > 2048
                || (encoded.length() & 1) != 0) {
            throw new IllegalArgumentException();
        }
        byte[] bytes = new byte[encoded.length() / 2];
        for (int i = 0; i < bytes.length; i++) {
            int high = Character.digit(encoded.charAt(i * 2), 16);
            int low = Character.digit(encoded.charAt(i * 2 + 1), 16);
            if (high < 0 || low < 0) throw new IllegalArgumentException();
            bytes[i] = (byte) ((high << 4) | low);
        }
        String title = StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
                .decode(ByteBuffer.wrap(bytes)).toString();
        if (title.codePointCount(0, title.length()) > 256) throw new IllegalArgumentException();
        for (int offset = 0; offset < title.length();) {
            int point = title.codePointAt(offset);
            if (Character.isISOControl(point)) throw new IllegalArgumentException();
            offset += Character.charCount(point);
        }
        return title;
    }

    public byte[] transform(ClassLoader loader, String name, Class<?> redefining,
            ProtectionDomain domain, byte[] original) {
        if (!"org/lwjgl/glfw/GLFW".equals(name)
                && !"org/lwjgl/opengl/Display".equals(name)) return null;
        try {
            byte[] changed = TitleTransformer.transform(name, original);
            if (changed == null) report("No supported title entry point was found.");
            else report("Custom game window title enabled.");
            return changed;
        } catch (Exception error) {
            report("Unsupported graphics class; original window title retained.");
            return null;
        } catch (LinkageError error) {
            report("Window title bridge unavailable; original window title retained.");
            return null;
        }
    }

    private synchronized void report(String message) {
        if (!reported) {
            reported = true;
            System.err.println("[PCL-RH title] " + message);
        }
    }
}
