package org.lwjgl.opengl;

public final class Display {
    public static String received;
    public static long thread;

    public static synchronized void setTitle(String title) {
        try {
            if (title == null) throw new IllegalArgumentException();
            received = title;
            thread = Thread.currentThread().getId();
        } catch (RuntimeException error) {
            received = "caught";
        }
    }
}
