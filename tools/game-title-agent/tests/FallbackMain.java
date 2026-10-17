import org.lwjgl.opengl.Display;

/** A cosmetic agent initialization/transform error must let the game continue. */
public final class FallbackMain {
    public static void main(String[] args) {
        Display.setTitle("original title");
        if (!"original title".equals(Display.received)) throw new AssertionError();
        System.out.println("Original title retained after an unsupported cosmetic edit.");
    }
}
