#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Gianluca Boiano
# SPDX-License-Identifier: GPL-3.0-only
"""Writes `classes.dex`, the only Java-side code of the Android build.

`cargo apk` cannot package Java classes, and Android needs real classes for
two things the NativeActivity cannot do from Rust over JNI: a foreground
`Service` (Android 14 requires a `mediaPlayback` one to keep audio alive in the
background) and a `MediaSession.Callback` (an abstract class: transport
buttons, headset keys). Instead of a Gradle/javac/d8 toolchain, this script
assembles the dex by hand (header, id tables, class_defs, code_items with
hand-assembled Dalvik bytecode, adler32 + SHA-1) and `check_dex.py`
re-reads and validates the result.

The classes are exactly this Java (package `io.github.m0rf30.aulos`):

    public class MediaService extends android.app.Service {
        /** Set by Rust before `startForegroundService`. */
        public static volatile Notification notification;

        /** Implemented in Rust (`android/media.rs`), registered with
         *  `RegisterNatives`: 1 play, 2 pause, 3 stop, 4 next, 5 previous,
         *  6 seek (`arg` = milliseconds). */
        public static native void nativeCommand(int command, long arg);

        public IBinder onBind(Intent intent) { return null; }

        public int onStartCommand(Intent intent, int flags, int startId) {
            int command = intent == null ? 0 : intent.getIntExtra("cmd", 0);
            if (command == 0) {
                // Manifest-declared foregroundServiceType (mediaPlayback).
                startForeground(1, notification);
            } else {
                nativeCommand(command, 0L);   // a notification button
            }
            return Service.START_NOT_STICKY; // 2
        }
    }

    public class MediaCallback extends android.media.session.MediaSession.Callback {
        public void onPlay()           { MediaService.nativeCommand(1, 0L); }
        public void onPause()          { MediaService.nativeCommand(2, 0L); }
        public void onStop()           { MediaService.nativeCommand(3, 0L); }
        public void onSkipToNext()     { MediaService.nativeCommand(4, 0L); }
        public void onSkipToPrevious() { MediaService.nativeCommand(5, 0L); }
        public void onSeekTo(long pos) { MediaService.nativeCommand(6, pos); }
    }

Run `python3 crates/aulos/android/build_dex.py` (no dependencies); the output
is committed as `classes.dex` next to this file and the build is
deterministic, so a clean regeneration leaves `git status` untouched.
"""

import hashlib
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT = HERE / "classes.dex"

PKG = "io/github/m0rf30/aulos/"
SERVICE = f"L{PKG}MediaService;"
CALLBACK = f"L{PKG}MediaCallback;"
FRAMEWORK_SERVICE = "Landroid/app/Service;"
FRAMEWORK_CALLBACK = "Landroid/media/session/MediaSession$Callback;"
NOTIFICATION = "Landroid/app/Notification;"
INTENT = "Landroid/content/Intent;"
IBINDER = "Landroid/os/IBinder;"
STRING = "Ljava/lang/String;"

ACC_PUBLIC, ACC_STATIC, ACC_VOLATILE = 0x1, 0x8, 0x40
ACC_NATIVE, ACC_CONSTRUCTOR = 0x100, 0x10000

NO_INDEX = 0xFFFFFFFF
START_NOT_STICKY = 2

# Map item type codes (dex format, "type codes").
T_HEADER, T_STRING_ID, T_TYPE_ID, T_PROTO_ID = 0x0000, 0x0001, 0x0002, 0x0003
T_FIELD_ID, T_METHOD_ID, T_CLASS_DEF = 0x0004, 0x0005, 0x0006
T_MAP_LIST, T_TYPE_LIST, T_CLASS_DATA, T_CODE = 0x1000, 0x1001, 0x2000, 0x2001
T_STRING_DATA = 0x2002


def uleb(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def shorty_char(descriptor):
    return "L" if descriptor[0] in "L[" else descriptor


class Method:
    """A method reference (class, name, return type, parameter types)."""

    def __init__(self, cls, name, ret, params=()):
        self.cls, self.name, self.ret, self.params = cls, name, ret, tuple(params)

    @property
    def shorty(self):
        return shorty_char(self.ret) + "".join(shorty_char(p) for p in self.params)

    def key(self):
        return (self.cls, self.name, self.ret, self.params)


class Field:
    def __init__(self, cls, name, type_):
        self.cls, self.name, self.type = cls, name, type_


# Framework and own methods the code refers to.
SERVICE_INIT = Method(FRAMEWORK_SERVICE, "<init>", "V")
START_FOREGROUND = Method(FRAMEWORK_SERVICE, "startForeground", "V", ["I", NOTIFICATION])
GET_INT_EXTRA = Method(INTENT, "getIntExtra", "I", [STRING, "I"])
CALLBACK_INIT = Method(FRAMEWORK_CALLBACK, "<init>", "V")
NATIVE_COMMAND = Method(SERVICE, "nativeCommand", "V", ["I", "J"])
NOTIFICATION_FIELD = Field(SERVICE, "notification", NOTIFICATION)


class Asm:
    """A tiny Dalvik assembler: only the opcodes this file needs.

    Registers are plain ints (`v0`, `v1`, ...). Branches use labels; offsets
    are resolved in a second pass. Operands that reference the id tables are
    `("str", s)`, `("meth", Method)`, `("field", Field)` and are resolved by
    `Dex.resolve` once indices are final.
    """

    def __init__(self):
        self.items = []  # ("label", name) | ("insn", size, fn)

    def label(self, name):
        self.items.append(("label", name))

    def _emit(self, size, encode):
        self.items.append(("insn", size, encode))

    # --- instructions -----------------------------------------------------
    def const4(self, reg, value):
        assert 0 <= reg < 16 and -8 <= value < 8
        self._emit(1, lambda ctx: [((value & 0xF) << 12) | (reg << 8) | 0x12])

    def const_wide16(self, reg, value):
        assert -0x8000 <= value < 0x8000
        self._emit(2, lambda ctx: [(reg << 8) | 0x16, value & 0xFFFF])

    def const_string(self, reg, string):
        self._emit(2, lambda ctx: [(reg << 8) | 0x1A, ctx.string(string)])

    def sget_object(self, reg, field):
        self._emit(2, lambda ctx: [(reg << 8) | 0x62, ctx.field(field)])

    def move_result(self, reg):
        self._emit(1, lambda ctx: [(reg << 8) | 0x0A])

    def return_void(self):
        self._emit(1, lambda ctx: [0x0E])

    def return_(self, reg):
        self._emit(1, lambda ctx: [(reg << 8) | 0x0F])

    def return_object(self, reg):
        self._emit(1, lambda ctx: [(reg << 8) | 0x11])

    def invoke(self, kind, regs, method):
        opcode = {"virtual": 0x6E, "direct": 0x70, "static": 0x71}[kind]
        n = len(regs)
        assert n <= 5 and all(0 <= r < 16 for r in regs)
        padded = list(regs) + [0] * (5 - n)
        c, d, e, f, g = padded
        first = (n << 12) | (g << 8) | opcode
        last = (f << 12) | (e << 8) | (d << 4) | c
        self._emit(3, lambda ctx: [first, ctx.method(method), last])

    def if_eqz(self, reg, label):
        self._emit(2, ("branch", 0x38, reg, label))

    def goto(self, label):
        self._emit(1, ("branch", 0x28, 0, label))

    # --- assembly ----------------------------------------------------------
    def assemble(self, ctx):
        # Pass 1: label addresses (in 16-bit code units).
        labels, address = {}, 0
        for item in self.items:
            if item[0] == "label":
                labels[item[1]] = address
            else:
                address += item[1]
        # Pass 2: encode.
        words, address = [], 0
        for item in self.items:
            if item[0] == "label":
                continue
            _, size, encode = item
            if isinstance(encode, tuple):
                _, opcode, reg, label = encode
                offset = labels[label] - address
                if opcode == 0x38:  # if-eqz vAA, +BBBB
                    assert -0x8000 <= offset < 0x8000
                    out = [(reg << 8) | opcode, offset & 0xFFFF]
                else:  # goto +AA
                    assert -128 <= offset < 128
                    out = [((offset & 0xFF) << 8) | opcode]
            else:
                out = encode(ctx)
            assert len(out) == size, (out, size)
            words += out
            address += size
        return words


class Code:
    def __init__(self, registers, ins, outs, asm):
        self.registers, self.ins, self.outs, self.asm = registers, ins, outs, asm


class EncodedMethod:
    def __init__(self, method, access, code=None):
        self.method, self.access, self.code = method, access, code


class ClassDef:
    def __init__(self, descriptor, superclass, static_fields, direct, virtual):
        self.descriptor, self.superclass = descriptor, superclass
        self.static_fields, self.direct, self.virtual = static_fields, direct, virtual


# --- the code ---------------------------------------------------------------
def init_code(super_init):
    asm = Asm()
    asm.invoke("direct", [0], super_init)
    asm.return_void()
    return Code(registers=1, ins=1, outs=1, asm=asm)


def on_bind():
    asm = Asm()
    asm.const4(0, 0)
    asm.return_object(0)
    return Code(registers=3, ins=2, outs=0, asm=asm)


def on_start_command():
    # v0..v2 locals; v3 this, v4 intent, v5 flags, v6 startId.
    this, intent = 3, 4
    asm = Asm()
    asm.if_eqz(intent, "start")
    asm.const_string(0, "cmd")
    asm.const4(1, 0)
    asm.invoke("virtual", [intent, 0, 1], GET_INT_EXTRA)
    asm.move_result(0)
    asm.if_eqz(0, "start")
    asm.const_wide16(1, 0)
    asm.invoke("static", [0, 1, 2], NATIVE_COMMAND)
    asm.goto("done")
    asm.label("start")
    asm.sget_object(0, NOTIFICATION_FIELD)
    asm.const4(1, 1)
    asm.invoke("virtual", [this, 1, 0], START_FOREGROUND)
    asm.label("done")
    asm.const4(0, START_NOT_STICKY)
    asm.return_(0)
    return Code(registers=7, ins=4, outs=3, asm=asm)


def forward(command):
    # v0..v2 locals; v3 this.
    asm = Asm()
    asm.const4(0, command)
    asm.const_wide16(1, 0)
    asm.invoke("static", [0, 1, 2], NATIVE_COMMAND)
    asm.return_void()
    return Code(registers=4, ins=1, outs=3, asm=asm)


def on_seek_to():
    # v0 local; v1 this; v2/v3 the position.
    asm = Asm()
    asm.const4(0, 6)
    asm.invoke("static", [0, 2, 3], NATIVE_COMMAND)
    asm.return_void()
    return Code(registers=4, ins=3, outs=3, asm=asm)


def classes():
    def override(name, code, params=()):
        return EncodedMethod(Method(CALLBACK, name, "V", params), ACC_PUBLIC, code)

    service = ClassDef(
        SERVICE,
        FRAMEWORK_SERVICE,
        static_fields=[(NOTIFICATION_FIELD, ACC_PUBLIC | ACC_STATIC | ACC_VOLATILE)],
        direct=[
            EncodedMethod(
                Method(SERVICE, "<init>", "V"),
                ACC_PUBLIC | ACC_CONSTRUCTOR,
                init_code(SERVICE_INIT),
            ),
            EncodedMethod(NATIVE_COMMAND, ACC_PUBLIC | ACC_STATIC | ACC_NATIVE),
        ],
        virtual=[
            EncodedMethod(
                Method(SERVICE, "onBind", IBINDER, [INTENT]), ACC_PUBLIC, on_bind()
            ),
            EncodedMethod(
                Method(SERVICE, "onStartCommand", "I", [INTENT, "I", "I"]),
                ACC_PUBLIC,
                on_start_command(),
            ),
        ],
    )
    callback = ClassDef(
        CALLBACK,
        FRAMEWORK_CALLBACK,
        static_fields=[],
        direct=[
            EncodedMethod(
                Method(CALLBACK, "<init>", "V"),
                ACC_PUBLIC | ACC_CONSTRUCTOR,
                init_code(CALLBACK_INIT),
            )
        ],
        virtual=[
            override("onPlay", forward(1)),
            override("onPause", forward(2)),
            override("onStop", forward(3)),
            override("onSkipToNext", forward(4)),
            override("onSkipToPrevious", forward(5)),
            override("onSeekTo", on_seek_to(), ["J"]),
        ],
    )
    return [service, callback]


# --- the writer ---------------------------------------------------------------
def mutf8(text):
    data = text.encode("utf-8")  # all names here are ASCII
    assert all(0 < b < 0x80 for b in data)
    return data


class Dex:
    def __init__(self, class_defs):
        self.class_defs = class_defs
        self.collect()

    def collect(self):
        methods, fields, types = {}, {}, set()
        strings = set()

        add_type = types.add

        def add_method(m):
            methods[m.key()] = m
            add_type(m.cls)
            add_type(m.ret)
            for p in m.params:
                add_type(p)
            strings.update([m.name, m.shorty])

        def add_field(f):
            fields[(f.cls, f.name, f.type)] = f
            add_type(f.cls)
            add_type(f.type)
            strings.add(f.name)

        for c in self.class_defs:
            add_type(c.descriptor)
            add_type(c.superclass)
            for f, _ in c.static_fields:
                add_field(f)
            for em in c.direct + c.virtual:
                add_method(em.method)
        # References made by the code itself.
        for c in self.class_defs:
            for em in c.direct + c.virtual:
                if em.code:
                    self.scan_code(em.code, add_method, add_field, strings)
        strings.update(types)
        self.strings = sorted(strings, key=lambda s: s.encode("utf-16-be"))
        self.string_index = {s: i for i, s in enumerate(self.strings)}
        self.types = sorted(types, key=lambda t: self.string_index[t])
        self.type_index = {t: i for i, t in enumerate(self.types)}
        protos = {(m.ret, m.params) for m in methods.values()}
        self.protos = sorted(
            protos,
            key=lambda p: (
                self.type_index[p[0]],
                [self.type_index[t] for t in p[1]],
            ),
        )
        self.proto_index = {p: i for i, p in enumerate(self.protos)}
        self.fields = sorted(
            fields.values(),
            key=lambda f: (
                self.type_index[f.cls],
                self.string_index[f.name],
                self.type_index[f.type],
            ),
        )
        self.field_index = {
            (f.cls, f.name, f.type): i for i, f in enumerate(self.fields)
        }
        self.methods = sorted(
            methods.values(),
            key=lambda m: (
                self.type_index[m.cls],
                self.string_index[m.name],
                self.proto_index[(m.ret, m.params)],
            ),
        )
        self.method_index = {m.key(): i for i, m in enumerate(self.methods)}

    def scan_code(self, code, add_method, add_field, strings):
        """Finds the references a code item makes (by dry-running the encoders)."""

        class Probe:
            def string(_, s):
                strings.add(s)
                return 0

            def field(_, f):
                add_field(f)
                return 0

            def method(_, m):
                add_method(m)
                return 0

        code.asm.assemble(Probe())

    # Index resolvers used while assembling for real.
    def string(self, s):
        return self.string_index[s]

    def field(self, f):
        return self.field_index[(f.cls, f.name, f.type)]

    def method(self, m):
        return self.method_index[m.key()]

    def build(self):
        string_ids_off = 0x70
        type_ids_off = string_ids_off + 4 * len(self.strings)
        proto_ids_off = type_ids_off + 4 * len(self.types)
        field_ids_off = proto_ids_off + 12 * len(self.protos)
        method_ids_off = field_ids_off + 8 * len(self.fields)
        class_defs_off = method_ids_off + 8 * len(self.methods)
        data_off = class_defs_off + 32 * len(self.class_defs)

        data = bytearray()

        def align(n):
            while (data_off + len(data)) % n:
                data.append(0)

        # string_data_item
        string_offs = []
        string_data_start = data_off + len(data)
        for s in self.strings:
            string_offs.append(data_off + len(data))
            data += uleb(len(s)) + mutf8(s) + b"\0"

        # type_list (4-aligned), shared between identical parameter lists
        align(4)
        type_list_start = data_off + len(data)
        type_lists, type_list_count = {}, 0
        for ret, params in self.protos:
            if params and params not in type_lists:
                align(4)
                type_lists[params] = data_off + len(data)
                type_list_count += 1
                data += struct.pack("<I", len(params))
                for t in params:
                    data += struct.pack("<H", self.type_index[t])
        align(4)

        # code_item (4-aligned)
        code_start = data_off + len(data)
        code_offs, code_count = {}, 0
        for c in self.class_defs:
            for em in c.direct + c.virtual:
                if not em.code:
                    continue
                align(4)
                code_offs[em.method.key()] = data_off + len(data)
                code_count += 1
                words = em.code.asm.assemble(self)
                data += struct.pack(
                    "<HHHHII",
                    em.code.registers,
                    em.code.ins,
                    em.code.outs,
                    0,
                    0,
                    len(words),
                )
                data += struct.pack(f"<{len(words)}H", *words)

        # class_data_item
        class_data_start = data_off + len(data)
        class_data_offs = []
        for c in self.class_defs:
            class_data_offs.append(data_off + len(data))
            data += uleb(len(c.static_fields)) + uleb(0)
            data += uleb(len(c.direct)) + uleb(len(c.virtual))
            previous = 0
            for f, access in sorted(
                c.static_fields, key=lambda fa: self.field(fa[0])
            ):
                index = self.field(f)
                data += uleb(index - previous) + uleb(access)
                previous = index
            for group in (c.direct, c.virtual):
                previous = 0
                for em in sorted(group, key=lambda e: self.method(e.method)):
                    index = self.method(em.method)
                    data += uleb(index - previous) + uleb(em.access)
                    data += uleb(code_offs.get(em.method.key(), 0))
                    previous = index

        # map_list (4-aligned)
        align(4)
        map_off = data_off + len(data)
        map_entries = [
            (T_HEADER, 1, 0),
            (T_STRING_ID, len(self.strings), string_ids_off),
            (T_TYPE_ID, len(self.types), type_ids_off),
            (T_PROTO_ID, len(self.protos), proto_ids_off),
            (T_FIELD_ID, len(self.fields), field_ids_off),
            (T_METHOD_ID, len(self.methods), method_ids_off),
            (T_CLASS_DEF, len(self.class_defs), class_defs_off),
            (T_STRING_DATA, len(self.strings), string_data_start),
            (T_TYPE_LIST, type_list_count, type_list_start),
            (T_CODE, code_count, code_start),
            (T_CLASS_DATA, len(self.class_defs), class_data_start),
            (T_MAP_LIST, 1, map_off),
        ]
        map_entries = [e for e in map_entries if e[1]]
        map_entries.sort(key=lambda e: e[2])
        data += struct.pack("<I", len(map_entries))
        for kind, size, offset in map_entries:
            data += struct.pack("<HHII", kind, 0, size, offset)

        # id tables
        tables = bytearray()
        for off in string_offs:
            tables += struct.pack("<I", off)
        for t in self.types:
            tables += struct.pack("<I", self.string_index[t])
        for ret, params in self.protos:
            shorty = shorty_char(ret) + "".join(shorty_char(p) for p in params)
            tables += struct.pack(
                "<III",
                self.string_index[shorty],
                self.type_index[ret],
                type_lists.get(params, 0),
            )
        for f in self.fields:
            tables += struct.pack(
                "<HHI",
                self.type_index[f.cls],
                self.type_index[f.type],
                self.string_index[f.name],
            )
        for m in self.methods:
            tables += struct.pack(
                "<HHI",
                self.type_index[m.cls],
                self.proto_index[(m.ret, m.params)],
                self.string_index[m.name],
            )
        for c, class_data in zip(self.class_defs, class_data_offs):
            tables += struct.pack(
                "<IIIIIIII",
                self.type_index[c.descriptor],
                ACC_PUBLIC,
                self.type_index[c.superclass],
                0,  # interfaces_off
                NO_INDEX,  # source_file_idx
                0,  # annotations_off
                class_data,
                0,  # static_values_off: every static starts null
            )
        assert len(tables) == data_off - 0x70

        file_size = data_off + len(data)
        header = bytearray(0x70)
        header[0:8] = b"dex\n035\0"
        struct.pack_into(
            "<IIIIIIIIIIIIIIIIII",
            header,
            32,
            file_size,
            0x70,
            0x12345678,
            0,  # link_size
            0,  # link_off
            map_off,
            len(self.strings),
            string_ids_off,
            len(self.types),
            type_ids_off,
            len(self.protos),
            proto_ids_off,
            len(self.fields),
            field_ids_off,
            len(self.methods),
            method_ids_off,
            len(self.class_defs),
            class_defs_off,
        )
        struct.pack_into("<II", header, 104, len(data), data_off)
        blob = bytearray(header) + tables + data
        assert len(blob) == file_size
        blob[12:32] = hashlib.sha1(bytes(blob[32:])).digest()
        struct.pack_into("<I", blob, 8, zlib.adler32(bytes(blob[12:])) & 0xFFFFFFFF)
        return bytes(blob)


def main():
    """`build_dex.py [output]`: writes `classes.dex` next to this script by default."""
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else OUT
    blob = Dex(classes()).build()
    out.write_bytes(blob)
    print(f"wrote {out} ({len(blob)} bytes)")
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(HERE))
    import check_dex

    check_dex.check(blob, verbose=False)
    print("validated with check_dex.py")


if __name__ == "__main__":
    main()
