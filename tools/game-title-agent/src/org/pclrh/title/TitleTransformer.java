package org.pclrh.title;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;

/**
 * A deliberately narrow class-file edit, without an ASM version dependency.
 * An eight-byte prefix replaces the title local and preserves switch alignment.
 * All old branch displacements remain valid; absolute code offsets and the
 * first stack-map delta move by eight. Unknown code attributes fail open to the
 * original class instead of guessing how to relocate their offsets.
 */
public final class TitleTransformer {
    private static final int PREFIX = 8;
    private final DataInputStream in;
    private final ByteArrayOutputStream bytes = new ByteArrayOutputStream();
    private final DataOutputStream out = new DataOutputStream(bytes);
    private String[] utf;
    private int methodReference;
    private boolean changed;
    private final boolean legacy;

    private TitleTransformer(String name, byte[] original) throws IOException {
        if (original.length > 16 * 1024 * 1024) throw new IOException();
        in = stream(original);
        legacy = name.equals("org/lwjgl/opengl/Display");
    }

    public static byte[] transform(String name, byte[] original) throws IOException {
        return new TitleTransformer(name, original).transform();
    }

    private byte[] transform() throws IOException {
        if (in.readInt() != 0xcafebabe) throw new IOException();
        out.writeInt(0xcafebabe);
        out.writeShort(in.readUnsignedShort());
        out.writeShort(in.readUnsignedShort());
        int count = in.readUnsignedShort();
        if (count > 65529) throw new IOException();
        utf = new String[count];
        out.writeShort(count + 6);
        for (int i = 1; i < count; i++) {
            int tag = in.readUnsignedByte();
            out.writeByte(tag);
            switch (tag) {
                case 1: utf[i] = in.readUTF(); out.writeUTF(utf[i]); break;
                case 3: case 4: copy(in, out, 4); break;
                case 5: case 6: copy(in, out, 8); i++; break;
                case 7: case 8: case 16: case 19: case 20: copy(in, out, 2); break;
                case 9: case 10: case 11: case 12: case 17: case 18: copy(in, out, 4); break;
                case 15: copy(in, out, 3); break;
                default: throw new IOException();
            }
        }
        String type = legacy ? "Ljava/lang/String;" : "Ljava/lang/CharSequence;";
        out.writeByte(1); out.writeUTF("org/pclrh/title/TitleText");
        out.writeByte(7); out.writeShort(count);
        out.writeByte(1); out.writeUTF("title");
        out.writeByte(1); out.writeUTF("(" + type + ")" + type);
        out.writeByte(12); out.writeShort(count + 2); out.writeShort(count + 3);
        out.writeByte(10); out.writeShort(count + 1); out.writeShort(count + 4);
        methodReference = count + 5;
        copy(in, out, 6); // access, this, super
        int interfaces = in.readUnsignedShort();
        out.writeShort(interfaces);
        copy(in, out, interfaces * 2);
        members(false);
        members(true);
        attributes(in, out, -1);
        if (in.available() != 0) throw new IOException();
        return changed ? bytes.toByteArray() : null;
    }

    private void members(boolean methods) throws IOException {
        int count = in.readUnsignedShort();
        out.writeShort(count);
        for (int i = 0; i < count; i++) {
            int access = in.readUnsignedShort();
            int name = in.readUnsignedShort();
            int descriptor = in.readUnsignedShort();
            out.writeShort(access); out.writeShort(name); out.writeShort(descriptor);
            int local = -1;
            if (methods && (access & 8) != 0) {
                if (legacy && "setTitle".equals(utf[name])
                        && "(Ljava/lang/String;)V".equals(utf[descriptor])) local = 0;
                if (!legacy && (("glfwCreateWindow".equals(utf[name])
                        && "(IILjava/lang/CharSequence;JJ)J".equals(utf[descriptor]))
                        || ("glfwSetWindowTitle".equals(utf[name])
                        && "(JLjava/lang/CharSequence;)V".equals(utf[descriptor])))) local = 2;
            }
            attributes(in, out, local);
        }
    }

    private void attributes(DataInputStream input, DataOutputStream output, int local)
            throws IOException {
        int count = input.readUnsignedShort();
        output.writeShort(count);
        for (int i = 0; i < count; i++) {
            int name = input.readUnsignedShort();
            byte[] value = read(input, input.readInt());
            if (local >= 0 && "Code".equals(utf[name])) {
                value = code(value, local);
                changed = true;
            }
            output.writeShort(name); output.writeInt(value.length); output.write(value);
        }
    }

    private byte[] code(byte[] value, int local) throws IOException {
        DataInputStream input = stream(value);
        ByteArrayOutputStream buffer = new ByteArrayOutputStream();
        DataOutputStream output = new DataOutputStream(buffer);
        output.writeShort(Math.max(1, input.readUnsignedShort()));
        int locals = input.readUnsignedShort();
        if (locals <= local) throw new IOException();
        output.writeShort(locals);
        int length = input.readInt();
        if (length < 0 || length > 65535 - PREFIX) throw new IOException();
        output.writeInt(length + PREFIX);
        output.writeByte(0x2a + local); // aload_<local>
        output.writeByte(0xb8); output.writeShort(methodReference);
        output.writeByte(0x4b + local); // astore_<local>
        output.write(new byte[3]); // eight bytes preserve switch padding
        copy(input, output, length);
        int exceptions = input.readUnsignedShort();
        output.writeShort(exceptions);
        for (int i = 0; i < exceptions; i++) {
            shifted(input, output); shifted(input, output); shifted(input, output);
            output.writeShort(input.readUnsignedShort());
        }
        int count = input.readUnsignedShort();
        output.writeShort(count);
        for (int i = 0; i < count; i++) {
            int name = input.readUnsignedShort();
            byte[] data = read(input, input.readInt());
            data = relocate(utf[name], data);
            output.writeShort(name); output.writeInt(data.length); output.write(data);
        }
        if (input.available() != 0) throw new IOException();
        return buffer.toByteArray();
    }

    private static byte[] relocate(String name, byte[] data) throws IOException {
        DataInputStream input = stream(data);
        ByteArrayOutputStream buffer = new ByteArrayOutputStream();
        DataOutputStream output = new DataOutputStream(buffer);
        int count = input.readUnsignedShort();
        output.writeShort(count);
        for (int i = 0; i < count; i++) {
            if ("StackMapTable".equals(name)) frame(input, output, i == 0 ? PREFIX : 0);
            else if ("LineNumberTable".equals(name)) {
                shifted(input, output); copy(input, output, 2);
            } else if ("LocalVariableTable".equals(name)
                    || "LocalVariableTypeTable".equals(name)) {
                shifted(input, output); copy(input, output, 8);
            } else throw new IOException();
        }
        // Reject even an empty unknown attribute: its offset convention is unknown.
        if (!("StackMapTable".equals(name) || "LineNumberTable".equals(name)
                || "LocalVariableTable".equals(name) || "LocalVariableTypeTable".equals(name))
                || input.available() != 0) throw new IOException();
        return buffer.toByteArray();
    }

    private static void frame(DataInputStream input, DataOutputStream output, int shift)
            throws IOException {
        int type = input.readUnsignedByte();
        if (type <= 63) {
            int delta = type + shift;
            if (delta <= 63) output.writeByte(delta);
            else { output.writeByte(251); output.writeShort(delta); }
        } else if (type <= 127) {
            int delta = type - 64 + shift;
            if (delta <= 63) output.writeByte(64 + delta);
            else { output.writeByte(247); output.writeShort(delta); }
            verification(input, output);
        } else {
            if (type < 247) throw new IOException();
            output.writeByte(type);
            int delta = input.readUnsignedShort() + shift;
            if (delta > 65535) throw new IOException();
            output.writeShort(delta);
            if (type == 247) verification(input, output);
            else if (type >= 252 && type <= 254) {
                for (int i = 0; i < type - 251; i++) verification(input, output);
            } else if (type == 255) {
                for (int part = 0; part < 2; part++) {
                    int count = input.readUnsignedShort();
                    output.writeShort(count);
                    for (int i = 0; i < count; i++) verification(input, output);
                }
            }
        }
    }

    private static void verification(DataInputStream input, DataOutputStream output)
            throws IOException {
        int tag = input.readUnsignedByte();
        if (tag > 8) throw new IOException();
        output.writeByte(tag);
        if (tag == 7) output.writeShort(input.readUnsignedShort());
        else if (tag == 8) shifted(input, output); // uninitialized new instruction
    }

    private static void shifted(DataInputStream input, DataOutputStream output)
            throws IOException {
        int offset = input.readUnsignedShort() + PREFIX;
        if (offset > 65535) throw new IOException();
        output.writeShort(offset);
    }

    private static DataInputStream stream(byte[] data) {
        return new DataInputStream(new ByteArrayInputStream(data));
    }

    private static byte[] read(DataInputStream input, int length) throws IOException {
        if (length < 0 || length > input.available()) throw new IOException();
        byte[] result = new byte[length]; input.readFully(result); return result;
    }

    private static void copy(DataInputStream input, DataOutputStream output, int length)
            throws IOException {
        output.write(read(input, length));
    }
}
