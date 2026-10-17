package org.pclrh.title;

/** Bootstrap-visible bridge: custom game class loaders can call this class. */
public final class TitleText {
    private static volatile String title;

    private TitleText() {}

    public static void configure(String value) {
        title = value;
    }

    public static String title(String original) {
        String configured = title;
        return configured == null ? original : configured;
    }

    public static CharSequence title(CharSequence original) {
        String configured = title;
        return configured == null ? original : configured;
    }
}
