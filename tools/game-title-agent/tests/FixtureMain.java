import java.io.File;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import org.lwjgl.glfw.GLFW;
import org.lwjgl.opengl.Display;

public final class FixtureMain {
    private static void equal(Object expected, Object actual) {
        if (!expected.equals(actual)) throw new AssertionError(expected + " != " + actual);
    }

    public static void main(String[] args) throws Exception {
        String expected = args[0];
        long thread = Thread.currentThread().getId();
        equal(42L, GLFW.glfwCreateWindow(200, 480, "original", 0, 0));
        equal(expected, GLFW.received);
        equal(thread, GLFW.thread);
        GLFW.glfwSetWindowTitle(2, (CharSequence) "game updated title");
        equal(expected, GLFW.received);
        GLFW.glfwSetWindowTitle(987, (CharSequence) "game updated title again");
        equal(expected, GLFW.received);
        equal(-1L, GLFW.glfwCreateWindow(17, 480, "original", 0, 0));
        equal(expected, GLFW.received);
        GLFW.glfwSetWindowTitle(1, new StringBuilder("special overload"));
        equal("untouched:special overload", GLFW.received);
        Display.setTitle("legacy original");
        equal(expected, Display.received);
        equal(thread, Display.thread);

        // No parent: the bridge must be visible from bootstrap, independently
        // of Minecraft/Forge/Fabric's application class-loader hierarchy.
        URL url = new File(args[1]).toURI().toURL();
        try (URLClassLoader isolated = new URLClassLoader(new URL[]{url}, null)) {
            Class<?> api = isolated.loadClass("org.lwjgl.glfw.GLFW");
            Method setter = api.getMethod("glfwSetWindowTitle", long.class, CharSequence.class);
            setter.invoke(null, 1L, "isolated original");
            equal(expected, api.getField("received").get(null));
            Class<?> legacy = isolated.loadClass("org.lwjgl.opengl.Display");
            legacy.getMethod("setTitle", String.class).invoke(null, "isolated legacy");
            equal(expected, legacy.getField("received").get(null));
        }
        System.out.println("Synthetic title APIs received the configured Unicode title on their caller thread.");
    }
}
