#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Gianluca Boiano
# SPDX-License-Identifier: GPL-3.0-only
"""Re-reads a `.dex` file and validates it structurally.

Usage: `python3 check_dex.py [classes.dex]` (prints a listing and exits
non-zero on the first problem). Checks the header, the Adler-32 and SHA-1
sums, that every id table is sorted and unique, that every index resolves,
that `class_data_item`s and `code_item`s decode, the map list against the
real layout and, for each code item, that the Dalvik instructions (the subset
`build_dex.py` emits) decode, branch to instruction boundaries, stay inside
the declared registers and pass the argument words their callee wants.
Standard library only.
"""

import hashlib
import struct
import sys
import zlib
from pathlib import Path

NO_INDEX = 0xFFFFFFFF
TYPE_NAMES = {
    0x0000: "header",
    0x0001: "string_id",
    0x0002: "type_id",
    0x0003: "proto_id",
    0x0004: "field_id",
    0x0005: "method_id",
    0x0006: "class_def",
    0x1000: "map_list",
    0x1001: "type_list",
    0x2000: "class_data",
    0x2001: "code_item",
    0x2002: "string_data",
}


class DexError(Exception):
    pass


def need(condition, message):
    if not condition:
        raise DexError(message)


def read_uleb(blob, pos):
    result, shift = 0, 0
    while True:
        byte = blob[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, pos
        shift += 7
        need(shift < 35, "uleb128 too long")


def type_words(descriptor):
    return 2 if descriptor in ("J", "D") else 1


def check(blob, verbose=True):
    log = print if verbose else (lambda *a, **k: None)
    need(len(blob) >= 0x70, "shorter than the header")
    need(blob[:8] == b"dex\n035\0", f"bad magic {blob[:8]!r}")
    (checksum,) = struct.unpack_from("<I", blob, 8)
    need(zlib.adler32(blob[12:]) & 0xFFFFFFFF == checksum, "adler32 mismatch")
    need(hashlib.sha1(blob[32:]).digest() == blob[12:32], "SHA-1 mismatch")
    (
        file_size,
        header_size,
        endian,
        link_size,
        link_off,
        map_off,
        n_strings,
        strings_off,
        n_types,
        types_off,
        n_protos,
        protos_off,
        n_fields,
        fields_off,
        n_methods,
        methods_off,
        n_classes,
        classes_off,
        data_size,
        data_off,
    ) = struct.unpack_from("<20I", blob, 32)
    need(file_size == len(blob), f"file_size {file_size} != {len(blob)}")
    need(header_size == 0x70, "header_size")
    need(endian == 0x12345678, "endian_tag")
    need(link_size == 0 and link_off == 0, "link section not expected")
    need(data_off + data_size == len(blob), "data section does not end the file")
    need(strings_off == 0x70, "string_ids must follow the header")
    need(types_off == strings_off + 4 * n_strings, "type_ids offset")
    need(protos_off == types_off + 4 * n_types, "proto_ids offset")
    need(fields_off == protos_off + 12 * n_protos, "field_ids offset")
    need(methods_off == fields_off + 8 * n_fields, "method_ids offset")
    need(classes_off == methods_off + 8 * n_methods, "class_defs offset")
    need(data_off == classes_off + 32 * n_classes, "data offset")
    log(f"header ok: {len(blob)} bytes, adler32 {checksum:#010x}, sha1 ok")

    # strings
    strings, string_offs = [], []
    for i in range(n_strings):
        (off,) = struct.unpack_from("<I", blob, strings_off + 4 * i)
        need(data_off <= off < len(blob), f"string {i} offset")
        string_offs.append(off)
        length, pos = read_uleb(blob, off)
        end = blob.index(b"\0", pos)
        text = blob[pos:end].decode("utf-8")
        need(len(text) == length, f"string {i} utf16 length {length} != {len(text)}")
        strings.append(text)
    need(
        strings == sorted(strings, key=lambda s: s.encode("utf-16-be")),
        "strings not sorted",
    )
    need(len(set(strings)) == len(strings), "duplicate strings")

    # types
    types = []
    for i in range(n_types):
        (idx,) = struct.unpack_from("<I", blob, types_off + 4 * i)
        need(idx < n_strings, f"type {i} string index")
        descriptor = strings[idx]
        ok = descriptor in set("VZBSCIJFD") or (
            descriptor[0] in "L[" and (descriptor[0] == "[" or descriptor.endswith(";"))
        )
        need(ok, f"bad type descriptor {descriptor!r}")
        types.append(descriptor)
    need(
        [struct.unpack_from("<I", blob, types_off + 4 * i)[0] for i in range(n_types)]
        == sorted(
            struct.unpack_from("<I", blob, types_off + 4 * i)[0] for i in range(n_types)
        ),
        "type_ids not sorted",
    )
    need(len(set(types)) == len(types), "duplicate types")

    def short(descriptor):
        return "L" if descriptor[0] in "L[" else descriptor

    # protos
    protos, type_list_offs = [], set()
    previous = None
    for i in range(n_protos):
        shorty_idx, ret_idx, params_off = struct.unpack_from(
            "<III", blob, protos_off + 12 * i
        )
        need(shorty_idx < n_strings and ret_idx < n_types, f"proto {i} indices")
        params = []
        if params_off:
            need(params_off % 4 == 0 and params_off >= data_off, "type_list offset")
            type_list_offs.add(params_off)
            (count,) = struct.unpack_from("<I", blob, params_off)
            need(count > 0, "empty type_list")
            ids = struct.unpack_from(f"<{count}H", blob, params_off + 4)
            need(all(t < n_types for t in ids), "type_list index")
            params = [types[t] for t in ids]
        need(
            strings[shorty_idx] == short(types[ret_idx]) + "".join(map(short, params)),
            f"proto {i} shorty {strings[shorty_idx]!r} does not match",
        )
        key = (ret_idx, tuple(types.index(p) for p in params))
        need(previous is None or previous < key, "proto_ids not sorted/unique")
        previous = key
        protos.append((types[ret_idx], params))

    # fields and methods
    def descriptor_of(index):
        return types[index]

    fields, previous = [], None
    for i in range(n_fields):
        cls, type_, name = struct.unpack_from("<HHI", blob, fields_off + 8 * i)
        need(cls < n_types and type_ < n_types and name < n_strings, f"field {i}")
        key = (cls, name, type_)
        need(previous is None or previous < key, "field_ids not sorted/unique")
        previous = key
        fields.append((types[cls], strings[name], types[type_]))
    methods, previous = [], None
    for i in range(n_methods):
        cls, proto, name = struct.unpack_from("<HHI", blob, methods_off + 8 * i)
        need(cls < n_types and proto < n_protos and name < n_strings, f"method {i}")
        key = (cls, name, proto)
        need(previous is None or previous < key, "method_ids not sorted/unique")
        previous = key
        ret, params = protos[proto]
        need(
            types[cls][0] == "L" or types[cls][0] == "[",
            "method on a primitive type",
        )
        methods.append((types[cls], strings[name], ret, params))

    def method_text(index):
        cls, name, ret, params = methods[index]
        return f"{cls}->{name}({''.join(params)}){ret}"

    # classes
    defined = set()
    code_offs, class_data_offs = [], []
    for i in range(n_classes):
        (
            cls,
            access,
            superclass,
            interfaces_off,
            source_file,
            annotations_off,
            class_data_off,
            static_values_off,
        ) = struct.unpack_from("<8I", blob, classes_off + 32 * i)
        need(cls < n_types, "class type index")
        need(superclass == NO_INDEX or superclass < n_types, "superclass index")
        need(interfaces_off == 0 and annotations_off == 0, "unexpected sections")
        need(source_file == NO_INDEX or source_file < n_strings, "source file")
        need(static_values_off == 0, "static values not expected")
        descriptor = types[cls]
        need(descriptor not in defined, f"class {descriptor} defined twice")
        defined.add(descriptor)
        log(f"class {descriptor} extends {types[superclass]} (access {access:#x})")
        need(class_data_off >= data_off, "class_data offset")
        class_data_offs.append(class_data_off)
        pos = class_data_off
        n_static, pos = read_uleb(blob, pos)
        n_instance, pos = read_uleb(blob, pos)
        n_direct, pos = read_uleb(blob, pos)
        n_virtual, pos = read_uleb(blob, pos)
        for kind, count in (("static field", n_static), ("instance field", n_instance)):
            index = 0
            for _ in range(count):
                diff, pos = read_uleb(blob, pos)
                flags, pos = read_uleb(blob, pos)
                index += diff
                need(index < n_fields, f"{kind} index")
                need(fields[index][0] == descriptor, f"{kind} of another class")
                log(f"  {kind} {fields[index][1]}: {fields[index][2]} ({flags:#x})")
        for kind, count in (("direct", n_direct), ("virtual", n_virtual)):
            index = 0
            for _ in range(count):
                diff, pos = read_uleb(blob, pos)
                flags, pos = read_uleb(blob, pos)
                code_off, pos = read_uleb(blob, pos)
                index += diff
                need(index < n_methods, f"{kind} method index")
                need(methods[index][0] == descriptor, f"{kind} method of another class")
                native = bool(flags & 0x100)
                need(
                    (code_off == 0) == native,
                    f"{method_text(index)}: code_off and native flag disagree",
                )
                log(f"  {kind} {method_text(index)} ({flags:#x})")
                if code_off:
                    need(code_off % 4 == 0 and code_off >= data_off, "code offset")
                    code_offs.append(code_off)
                    check_code(blob, code_off, index, methods, strings, fields, types, log)

    # map list
    need(map_off % 4 == 0 and map_off >= data_off, "map_off")
    (count,) = struct.unpack_from("<I", blob, map_off)
    entries = [
        struct.unpack_from("<HHII", blob, map_off + 4 + 12 * i) for i in range(count)
    ]
    need(
        [e[3] for e in entries] == sorted(e[3] for e in entries),
        "map list not sorted by offset",
    )
    sizes = {e[0]: (e[2], e[3]) for e in entries}
    expected = {
        0x0000: (1, 0),
        0x0001: (n_strings, strings_off),
        0x0002: (n_types, types_off),
        0x0003: (n_protos, protos_off),
        0x0004: (n_fields, fields_off),
        0x0005: (n_methods, methods_off),
        0x0006: (n_classes, classes_off),
        0x1000: (1, map_off),
        0x1001: (len(type_list_offs), min(type_list_offs, default=0)),
        0x2000: (n_classes, min(class_data_offs, default=0)),
        0x2001: (len(code_offs), min(code_offs, default=0)),
        0x2002: (n_strings, min(string_offs, default=0)),
    }
    for kind, (size, offset) in expected.items():
        if size == 0:
            need(kind not in sizes, f"map has an empty {TYPE_NAMES[kind]}")
            continue
        need(kind in sizes, f"map lacks {TYPE_NAMES[kind]}")
        need(
            sizes[kind] == (size, offset),
            f"map {TYPE_NAMES[kind]}: {sizes[kind]} != {(size, offset)}",
        )
    need(set(sizes) == {k for k, v in expected.items() if v[0]}, "unexpected map items")
    log("map list ok")
    return {"strings": strings, "types": types, "methods": methods, "fields": fields}


# opcode -> (name, format)
OPCODES = {
    0x0A: ("move-result", "11x"),
    0x0E: ("return-void", "10x"),
    0x0F: ("return", "11x"),
    0x11: ("return-object", "11x"),
    0x12: ("const/4", "11n"),
    0x16: ("const-wide/16", "21s"),
    0x1A: ("const-string", "21c-string"),
    0x28: ("goto", "10t"),
    0x38: ("if-eqz", "21t"),
    0x62: ("sget-object", "21c-field"),
    0x6E: ("invoke-virtual", "35c"),
    0x70: ("invoke-direct", "35c"),
    0x71: ("invoke-static", "35c"),
}


def check_code(blob, off, method_index, methods, strings, fields, types, log):
    registers, ins, outs, tries, debug_off, size = struct.unpack_from("<HHHHII", blob, off)
    need(tries == 0 and debug_off == 0, "tries/debug info not expected")
    need(ins <= registers, "ins > registers")
    words = list(struct.unpack_from(f"<{size}H", blob, off + 16))
    cls, name, ret, params = methods[method_index]
    static = False
    # `static` is recorded in the access flags; infer the receiver from `ins`.
    expected_ins = sum(type_words(p) for p in params)
    need(ins in (expected_ins, expected_ins + 1), f"{name}: ins {ins} vs {expected_ins}")
    static = ins == expected_ins

    # Decode.
    insns, pc = {}, 0
    while pc < size:
        word = words[pc]
        opcode = word & 0xFF
        need(opcode in OPCODES, f"{name}@{pc}: unsupported opcode {opcode:#x}")
        mnemonic, fmt = OPCODES[opcode]
        if fmt in ("10x", "11x", "11n", "10t"):
            length = 1
        elif fmt in ("21s", "21c-string", "21c-field", "21t"):
            length = 2
        else:
            length = 3
        need(pc + length <= size, f"{name}@{pc}: truncated {mnemonic}")
        insns[pc] = (mnemonic, fmt, words[pc : pc + length])
        pc += length

    def reg(r):
        need(r < registers, f"{name}: v{r} outside {registers} registers")
        return f"v{r}"

    max_outs = 0
    last = None
    for pc, (mnemonic, fmt, w) in insns.items():
        last = mnemonic
        a = (w[0] >> 8) & 0xFF
        if fmt == "10x":
            text = mnemonic
        elif fmt == "11x":
            text = f"{mnemonic} {reg(a)}"
        elif fmt == "11n":
            lit = (w[0] >> 12) & 0xF
            lit = lit - 16 if lit > 7 else lit
            text = f"{mnemonic} {reg(a & 0xF)}, #{lit}"
        elif fmt == "10t":
            off_ = a - 256 if a > 127 else a
            need(pc + off_ in insns, f"{name}@{pc}: goto into the middle of nowhere")
            text = f"{mnemonic} :{pc + off_:04x}"
        elif fmt == "21s":
            lit = w[1] - 0x10000 if w[1] > 0x7FFF else w[1]
            need(a + 1 < registers, "wide register pair out of range")
            text = f"{mnemonic} {reg(a)}, #{lit}"
        elif fmt == "21t":
            off_ = w[1] - 0x10000 if w[1] > 0x7FFF else w[1]
            need(pc + off_ in insns, f"{name}@{pc}: branch to a non-instruction")
            text = f"{mnemonic} {reg(a)}, :{pc + off_:04x}"
        elif fmt == "21c-string":
            need(w[1] < len(strings), "string index")
            text = f"{mnemonic} {reg(a)}, {strings[w[1]]!r}"
        elif fmt == "21c-field":
            need(w[1] < len(fields), "field index")
            f = fields[w[1]]
            text = f"{mnemonic} {reg(a)}, {f[0]}->{f[1]}:{f[2]}"
        else:  # 35c
            count = (w[0] >> 12) & 0xF
            g = (w[0] >> 8) & 0xF
            need(w[1] < len(methods), "method index")
            c, d, e, f_ = w[2] & 0xF, (w[2] >> 4) & 0xF, (w[2] >> 8) & 0xF, (w[2] >> 12) & 0xF
            regs = [c, d, e, f_, g][:count]
            callee = methods[w[1]]
            callee_params = list(callee[3])
            receiver = mnemonic != "invoke-static"
            expected = ([callee[0]] if receiver else []) + callee_params
            need(
                sum(type_words(p) for p in expected) == count,
                f"{name}@{pc}: {count} argument words for {callee[1]}",
            )
            slot = 0
            for p in expected:
                if type_words(p) == 2:
                    need(
                        regs[slot + 1] == regs[slot] + 1,
                        f"{name}@{pc}: wide argument needs a register pair",
                    )
                slot += type_words(p)
            for r in regs:
                reg(r)
            max_outs = max(max_outs, count)
            text = f"{mnemonic} {{{', '.join('v%d' % r for r in regs)}}}, {method_text_of(callee)}"
        log(f"    {pc:04x}: {text}")
    need(outs >= max_outs, f"{name}: outs {outs} < {max_outs} used")
    need(last in ("return-void", "return", "return-object", "goto"), "falls off the end")
    log(f"    ({registers} registers, {ins} in, {outs} out, {size} units)")
    _ = static


def method_text_of(callee):
    cls, name, ret, params = callee
    return f"{cls}->{name}({''.join(params)}){ret}"


if __name__ == "__main__":
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).with_name("classes.dex")
    try:
        check(path.read_bytes())
    except DexError as error:
        print(f"INVALID: {error}", file=sys.stderr)
        sys.exit(1)
    print("OK")
