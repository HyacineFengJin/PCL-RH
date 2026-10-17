package org.lwjgl.glfw;

/** Synthetic graphics API: no native library, display, or game is loaded. */
public final class GLFW {
    public static String received;
    public static long thread;

    public static long glfwCreateWindow(int width, int height, CharSequence title,
            long monitor, long share) {
        // Both switch alignment and stack-map relocation matter here. A naive
        // instruction prefix breaks verification or dispatch before assertions.
        try {
            // A new object remains uninitialized across a branch in its
            // constructor argument; StackMapTable tag-8 offsets must relocate.
            StringBuilder witness = new StringBuilder(width == 200 ? title : title);
            switch (width) {
                case 100: received = title.toString() + height; break;
                case 200: received = witness.toString(); break;
                default: throw new IllegalArgumentException();
            }
            thread = Thread.currentThread().getId();
            return 42;
        } catch (IllegalArgumentException error) {
            received = title.toString();
            return -1;
        }
    }

    public static void glfwSetWindowTitle(long window, CharSequence title) {
        switch ((int) window) {
            case 1: case 2: case 3: received = title.toString(); break;
            default:
                if (title.length() < 1) throw new IllegalArgumentException();
                received = title.toString();
        }
        thread = Thread.currentThread().getId();
    }

    // A nonmatching overload is untouched, documenting the capability boundary.
    public static void glfwSetWindowTitle(long window, StringBuilder title) {
        received = "untouched:" + title.toString();
    }
}
