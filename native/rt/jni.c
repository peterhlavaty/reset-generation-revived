/* A fake JNIEnv for the engine: the function table lives in guest memory and points at host
 * stubs. Java objects are small integer handles into a host-side table. The engine's calls
 * back into "Java" (CallXxxMethod) are forwarded to rt_java_call() by method name.
 *
 * Handles live as long as a JVM would keep them, so the table never fills (it used to, after
 * about 8,150 received network messages: "JNI object table full"):
 * - classes, method and field IDs and the host's objects and buffers are permanent and
 *   interned: the same name (or buffer) gives the same handle, so asking again takes no slot;
 * - strings and the engine's own buffers are local references. A JVM frees those when the
 *   native method returns, and the engine relies on that: the DLL never calls DeleteLocalRef
 *   (the room string the host passes to onDataReceived for every received message, its own
 *   NewStringUTF for joinRoom/createRoom/leaveRoom, ...). We do not see native calls return,
 *   so locals take the slots of a ring in turn and a local dies when the ring comes round to
 *   its slot again, after LOCAL_SLOTS newer locals: far more than one native call holds.
 *   DeleteLocalRef frees one at once; NewGlobalRef keeps one until DeleteGlobalRef.
 * Both sides of the oracle run this code, so their handles stay identical. */
#include "host.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define JNI_BASE RT_JNI_BASE
#define OBJ_BASE 0x1000u
#define MAX_OBJS 8192
#define PERM_SLOTS 1024                     /* slots 1..1023 (0 is null); the engine needs about 30 */
#define LOCAL_SLOTS (MAX_OBJS - PERM_SLOTS) /* the ring of local references */

enum { O_FREE, O_CLASS, O_OBJECT, O_STRING, O_DBUF, O_METHOD, O_FIELD };
typedef struct {
    int kind;
    uint8_t local;      /* ring slots: the local reference is still live */
    uint32_t globals;   /* ring slots: NewGlobalRef count; keeps the object after its local died */
    char *str;          /* class name, string value, method name */
    char *sig;          /* method signature */
    uint32_t addr, len; /* direct buffer */
} jobj;
static jobj objs[MAX_OBJS];
static uint32_t nperm = 1, next_local = PERM_SLOTS;

/* the handle's slot, 0 if it is not a live object */
static uint32_t slot_of(uint32_t h) {
    uint32_t i = (h - OBJ_BASE) / 4;
    return h >= OBJ_BASE && i < MAX_OBJS && objs[i].kind != O_FREE ? i : 0;
}
static jobj *obj(uint32_t h) { uint32_t i = slot_of(h); return i ? &objs[i] : NULL; }

static int same(const char *a, const char *b) { return a == b || (a && b && !strcmp(a, b)); }

static void fill(jobj *o, int kind, const char *str, const char *sig, uint32_t addr, uint32_t len) {
    o->kind = kind;
    o->str = str ? strdup(str) : NULL;
    o->sig = sig ? strdup(sig) : NULL;
    o->addr = addr;
    o->len = len;
}

static void drop(jobj *o) { free(o->str); free(o->sig); memset(o, 0, sizeof *o); }

/* The permanent object of this kind with this name and signature (or buffer), made on first use. */
static uint32_t perm_obj(int kind, const char *str, const char *sig, uint32_t addr, uint32_t len) {
    for (uint32_t i = 1; i < nperm; i++) {
        const jobj *o = &objs[i];
        if (o->kind == kind && same(o->str, str) && same(o->sig, sig) && o->addr == addr && o->len == len)
            return OBJ_BASE + 4 * i;
    }
    if (nperm >= PERM_SLOTS) rt_fatal("JNI: more than %d classes, IDs and host objects", PERM_SLOTS - 1);
    fill(&objs[nperm], kind, str, sig, addr, len);
    return OBJ_BASE + 4 * nperm++;
}

/* A new local reference in the ring's next slot; the local that had it died long ago. */
static uint32_t new_local(int kind, const char *str, const char *sig, uint32_t addr, uint32_t len) {
    for (uint32_t n = 0; n < LOCAL_SLOTS; n++) {
        uint32_t i = next_local;
        next_local = i + 1 < MAX_OBJS ? i + 1 : PERM_SLOTS;
        jobj *o = &objs[i];
        if (o->kind != O_FREE) {
            o->local = 0;
            if (o->globals) continue;   /* a global reference still holds the object */
            drop(o);
        }
        fill(o, kind, str, sig, addr, len);
        o->local = 1;
        return OBJ_BASE + 4 * i;
    }
    rt_fatal("JNI object table full: all %d local slots are held by global references", LOCAL_SLOTS);
}

/* the natives' `this` objects and the empty friend-list arrays: one object per class */
uint32_t rt_jni_new_object(const char *kind) { return perm_obj(O_OBJECT, kind, NULL, 0, 0); }
/* a local reference, for the native call it is passed to (or returned from) */
uint32_t rt_jni_new_string(const char *s) { return new_local(O_STRING, s, NULL, 0, 0); }
/* the buffers the host keeps (frame, network, sound): one handle per buffer */
uint32_t rt_jni_new_direct_buffer(uint32_t a, uint32_t len) { return perm_obj(O_DBUF, NULL, NULL, a, len); }
const char *rt_jni_string(uint32_t h) { jobj *o = obj(h); return o && o->kind == O_STRING ? o->str : NULL; }
uint32_t rt_jni_env(void) { return JNI_BASE; }

/* the class a JVM reports for an object (a string's str is its text, not a class name) */
static const char *class_name(const jobj *o) {
    switch (o ? o->kind : O_FREE) {
    case O_OBJECT: return o->str ? o->str : "?";
    case O_STRING: return "java/lang/String";
    case O_DBUF: return "java/nio/DirectByteBuffer";
    case O_CLASS: return "java/lang/Class";
    default: return "?";
    }
}

/* GetStringUTFChars copies, in guest memory. onDataReceived (0x1010a760) reads the room name of
 * every received message and never releases it (under a JVM that leaked in the JVM's own heap),
 * so a fresh copy each time would leak 32-48 bytes of guest heap per message. JNI lets the chars
 * be shared (callers must not modify them): a text has one copy while anyone holds it, freed
 * when its last holder releases it. */
typedef struct { uint32_t p, refs; } held_chars;
static held_chars *held;
static uint32_t nheld, held_cap;

static uint32_t chars_get(const char *s) {
    for (uint32_t i = 0; i < nheld; i++)
        if (!strcmp((const char *)(M + held[i].p), s)) {
            if (held[i].refs != UINT32_MAX) held[i].refs++;   /* saturated: kept for good */
            return held[i].p;
        }
    if (nheld == held_cap) {
        held_cap = held_cap ? 2 * held_cap : 16;
        held = realloc(held, held_cap * sizeof *held);
        if (!held) rt_fatal("JNI: out of memory");
    }
    held[nheld].p = rt_strdup_guest(s);
    held[nheld].refs = 1;
    return held[nheld++].p;
}

static void chars_release(uint32_t p) {
    for (uint32_t i = 0; i < nheld; i++)
        if (held[i].p == p) {
            if (held[i].refs != UINT32_MAX && !--held[i].refs) { rt_heap_free(p); held[i] = held[--nheld]; }
            return;
        }
    rt_heap_free(p);   /* not from GetStringUTFChars: freed as before */
}

/* ------------------------------------------------------------------ JNI functions */
#define A(i) rt_arg(i)
#define JNI(name) static void j_##name(int idx)

JNI(GetVersion) { rt_ret(0x00010004, 1); }
JNI(FindClass) { rt_ret(perm_obj(O_CLASS, rt_guest_str(A(1)), NULL, 0, 0), 2); }
JNI(ExceptionOccurred) { rt_ret(0, 1); }
JNI(ExceptionDescribe) { rt_ret(0, 1); }
JNI(ExceptionClear) { rt_ret(0, 1); }
JNI(ExceptionCheck) { rt_ret(0, 1); }
JNI(PushLocalFrame) { rt_ret(0, 2); }
JNI(PopLocalFrame) { rt_ret(A(1), 2); }
JNI(EnsureLocalCapacity) { rt_ret(0, 2); }
JNI(NewGlobalRef) {
    uint32_t i = slot_of(A(1));
    if (i >= PERM_SLOTS) objs[i].globals++;   /* a local: keep it past its ring slot's turn */
    rt_ret(A(1), 2);
}
JNI(NewLocalRef) { rt_ret(A(1), 2); }
JNI(DeleteGlobalRef) {
    uint32_t i = slot_of(A(1));
    if (i >= PERM_SLOTS && objs[i].globals && !--objs[i].globals && !objs[i].local) drop(&objs[i]);
    rt_ret(0, 2);
}
JNI(DeleteLocalRef) {
    uint32_t i = slot_of(A(1));
    /* permanent objects (the natives' `this`, classes, IDs) stay; so does a globally held local */
    if (i >= PERM_SLOTS && objs[i].local) {
        objs[i].local = 0;
        if (!objs[i].globals) drop(&objs[i]);
    }
    rt_ret(0, 2);
}
JNI(IsSameObject) { rt_ret(A(1) == A(2), 3); }
JNI(MonitorEnter) { rt_ret(0, 2); }
JNI(MonitorExit) { rt_ret(0, 2); }
JNI(GetObjectClass) { rt_ret(perm_obj(O_CLASS, class_name(obj(A(1))), NULL, 0, 0), 2); }
/* an ID names a method, not one class's method: rt_java_call dispatches on the object and name */
JNI(GetMethodID) { rt_ret(perm_obj(O_METHOD, rt_guest_str(A(2)), rt_guest_str(A(3)), 0, 0), 4); }
JNI(GetFieldID) { rt_ret(perm_obj(O_FIELD, rt_guest_str(A(2)), rt_guest_str(A(3)), 0, 0), 4); }
JNI(GetIntField) { rt_ret(0, 3); }

JNI(NewStringUTF) { rt_ret(A(1) ? rt_jni_new_string(rt_guest_str(A(1))) : 0, 2); }
JNI(GetStringUTFLength) { const char *s = rt_jni_string(A(1)); rt_ret(s ? (uint32_t)strlen(s) : 0, 2); }
JNI(GetStringUTFChars) {
    const char *s = rt_jni_string(A(1));
    if (A(2)) M[A(2)] = 1;
    rt_ret(chars_get(s ? s : ""), 3);
}
JNI(ReleaseStringUTFChars) { chars_release(A(2)); rt_ret(0, 3); }
JNI(GetStringLength) { const char *s = rt_jni_string(A(1)); rt_ret(s ? (uint32_t)strlen(s) : 0, 2); }
JNI(GetStringChars) {
    const char *s = rt_jni_string(A(1));
    if (!s) s = "";
    uint32_t n = (uint32_t)strlen(s), p = rt_heap_alloc(2 * n + 2);
    for (uint32_t i = 0; i <= n; i++) ST16(p + 2 * i, (uint8_t)s[i]);
    if (A(2)) M[A(2)] = 1;
    rt_ret(p, 3);
}
JNI(ReleaseStringChars) { rt_heap_free(A(2)); rt_ret(0, 3); }
JNI(GetStringUTFRegion) {
    const char *s = rt_jni_string(A(1));
    uint32_t start = A(2), len = A(3), buf = A(4);
    if (s) memcpy(M + buf, s + start, len);
    M[buf + len] = 0;
    rt_ret(0, 5);
}
JNI(GetArrayLength) { jobj *o = obj(A(1)); rt_ret(o ? o->len : 0, 2); }
/* arrays: only the empty friend-list arrays exist on our side */
JNI(GetObjectArrayElement) { rt_ret(0, 3); }
JNI(GetPrimitiveArrayElements) {
    jobj *o = obj(A(1));
    if (A(2)) M[A(2)] = 1;
    rt_ret(rt_heap_alloc(o && o->len ? o->len * 8 : 8), 3);
}
JNI(ReleasePrimitiveArrayElements) { rt_heap_free(A(2)); rt_ret(0, 4); }
JNI(NewDirectByteBuffer) { rt_ret(new_local(O_DBUF, NULL, NULL, A(1), A(2)), 4); }
JNI(GetDirectBufferAddress) { jobj *o = obj(A(1)); rt_ret(o && o->kind == O_DBUF ? o->addr : 0, 2); }
JNI(GetDirectBufferCapacity) { jobj *o = obj(A(1)); cpu.r[EDX] = 0; rt_ret(o && o->kind == O_DBUF ? o->len : 0, 2); }

/* Call<Type>Method[V|A] and CallStatic<Type>Method[V|A].
 * idx encodes the JNI table index; variant = (idx - first) % 3: 0 = varargs (cdecl),
 * 1 = va_list (stdcall, guest pointer), 2 = jvalue array (stdcall). */
static void call_method(int idx) {
    int is_static = idx >= 114;
    int rel = idx - (is_static ? 114 : 34);
    int type = rel / 3, variant = rel % 3;     /* Object Boolean Byte Char Short Int Long Float Double Void */
    uint32_t objh = A(1), mid = A(2);
    jobj *m = obj(mid);
    if (!m || m->kind != O_METHOD) rt_fatal("JNI call with bad method id %08x", mid);
    rt_jvalue args[16];
    int nargs = 0;
    uint32_t p = variant == 0 ? cpu.r[ESP] + 16 : A(3);   /* varargs start after env,obj,mid */
    const char *s = m->sig + 1;
    while (*s && *s != ')' && nargs < 16) {
        char c = *s;
        if (variant == 2) {
            uint32_t slot = p + 8 * (uint32_t)nargs;
            switch (c) {
            case 'Z': case 'B': args[nargs].i = (int8_t)M[slot]; break;
            case 'C': args[nargs].i = LD16(slot); break;
            case 'S': args[nargs].i = (int16_t)LD16(slot); break;
            case 'J': args[nargs].j = (int64_t)LD64(slot); break;
            case 'F': args[nargs].d = F32(LD32(slot)); break;
            case 'D': args[nargs].d = F64(LD64(slot)); break;
            default: args[nargs].i = (int32_t)LD32(slot); break;
            }
        } else if (c == 'J' || c == 'D' || c == 'F') {   /* floats are promoted to double in varargs */
            uint64_t v = LD64(p);
            p += 8;
            if (c == 'J') args[nargs].j = (int64_t)v; else args[nargs].d = F64(v);
        } else {
            args[nargs].i = (int32_t)LD32(p);
            p += 4;
        }
        nargs++;
        while (*s == '[') s++;
        if (*s == 'L') { while (*s && *s != ';') s++; }
        s++;
    }
    rt_jvalue r = rt_java_call(objh, m->str, m->sig, args, nargs);
    int pop = variant == 0 ? 0 : 4;
    if (type == 6) { cpu.r[EDX] = (uint32_t)((uint64_t)r.j >> 32); rt_ret((uint32_t)r.j, pop); }
    else if (type == 7 || type == 8) {
        for (int k = 7; k > 0; k--) RFD(0x1100 + 0x10 * k) = RFD(0x1100 + 0x10 * (k - 1));
        RFD(0x1100) = r.d;
        rt_ret(0, pop);
    } else rt_ret(type == 9 ? 0 : (uint32_t)r.i, pop);
}

static void unimplemented(int idx) { rt_fatal("JNI function #%d not implemented", idx); }

void rt_jni_init(void) {
    rt_mem_commit(JNI_BASE, 0x2000);
    uint32_t table = JNI_BASE + 0x100;
    ST32(JNI_BASE, table);
    static const struct { int idx; const char *name; rt_stub_fn fn; } fns[] = {
#define F(i, n) {i, #n, j_##n}
        F(4, GetVersion), F(6, FindClass), F(15, ExceptionOccurred), F(16, ExceptionDescribe),
        F(17, ExceptionClear), F(19, PushLocalFrame), F(20, PopLocalFrame), F(21, NewGlobalRef),
        F(22, DeleteGlobalRef), F(23, DeleteLocalRef), F(24, IsSameObject), F(25, NewLocalRef),
        F(26, EnsureLocalCapacity), F(31, GetObjectClass), F(33, GetMethodID), F(94, GetFieldID),
        F(100, GetIntField), F(113, GetMethodID), F(144, GetFieldID), F(164, GetStringLength),
        F(165, GetStringChars), F(166, ReleaseStringChars), F(167, NewStringUTF),
        F(168, GetStringUTFLength), F(169, GetStringUTFChars), F(170, ReleaseStringUTFChars),
        F(171, GetArrayLength), F(173, GetObjectArrayElement), F(217, MonitorEnter), F(218, MonitorExit), F(221, GetStringUTFRegion),
        F(228, ExceptionCheck), F(229, NewDirectByteBuffer), F(230, GetDirectBufferAddress),
        F(231, GetDirectBufferCapacity),
#undef F
    };
    static char names[240][40];
    for (int i = 0; i < 240; i++) {
        rt_stub_fn fn = unimplemented;
        const char *name = NULL;
        for (size_t k = 0; k < sizeof fns / sizeof fns[0]; k++) if (fns[k].idx == i) { fn = fns[k].fn; name = fns[k].name; }
        if ((i >= 34 && i <= 63) || (i >= 114 && i <= 143)) { fn = call_method; name = "CallMethod"; }
        if (i >= 183 && i <= 190) { fn = j_GetPrimitiveArrayElements; name = "GetArrayElements"; }
        if (i >= 191 && i <= 198) { fn = j_ReleasePrimitiveArrayElements; name = "ReleaseArrayElements"; }
        snprintf(names[i], sizeof names[i], "JNI_%d_%s", i, name ? name : "missing");
        ST32(table + 4 * (uint32_t)i, rt_stub_register(names[i], fn, i));
    }
}
