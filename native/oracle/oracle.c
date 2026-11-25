/* The oracle: the ORIGINAL engine DLL run on the real CPU, to check the recompiler against.
 *
 * A 32-bit Windows program (`cargo x oracle` builds it with rgtest.c as the driver and runs it
 * next to a recompiled rgtest). The DLL is mapped at its own base by the runtime's PE loader and
 * runs natively; every import and JNI call lands, through a trampoline, in the same Win32/JNI
 * layer (win32.c, jni.c) and engine glue (engine.c) the recompiled builds use. Guest addresses
 * are this process's addresses (M = 0, the compact map of host.h at its real place), the guest
 * runs on the guest stack at the same addresses as in rgtest, and the stubs sit at RT_STUB_BASE
 * as there, so the API trace (RT_API_TRACE) and the frame hashes of both must be identical. The
 * recompiled side is built without our layout patches and hooks (recomp --pure, RT_PURE), as the
 * original has none. */
#include "rt.h"
#include "host.h"
#include <windows.h>

#if !defined(_WIN32) || !defined(__i386__) || !defined(RT_ORACLE)
#error "the oracle is a 32-bit Windows program built with -DRT_ORACLE"
#endif

#define STUB_SLOTS 1025                   /* win32.c MAX_STUBS + 1; slot n at RT_STUB_BASE + 16 n */

uint32_t oracle_host_esp;                 /* the host's stack while guest code runs */
uint32_t oracle_enter(uint32_t addr, uint32_t ecx, uint32_t esp);
void oracle_stub_entry(void);

/* oracle_enter: runs guest code at `addr` on the guest stack `esp` (arguments already there; the
 * call pushes the return address below them), with ECX set; returns EAX.
 * oracle_stub_entry: where every stub slot jumps (EAX = the slot's address, the guest's return
 * address at [ESP]): back to the host stack, the stub runs (rt_stub_call reads its arguments from
 * cpu.r[ESP] and pops them with rt_ret), then on to the return address with EAX/EDX from cpu. */
__asm__(
    ".intel_syntax noprefix\n"
    ".text\n"
    ".globl _oracle_enter\n"
    "_oracle_enter:\n"
    "    push ebp\n"
    "    push ebx\n"
    "    push esi\n"
    "    push edi\n"
    "    mov eax, [esp + 20]\n"
    "    mov ecx, [esp + 24]\n"
    "    mov edx, [esp + 28]\n"
    "    mov [_oracle_host_esp], esp\n"
    "    mov esp, edx\n"
    "    cld\n"
    "    call eax\n"
    "    mov esp, [_oracle_host_esp]\n"
    "    pop edi\n"
    "    pop esi\n"
    "    pop ebx\n"
    "    pop ebp\n"
    "    ret\n"
    ".globl _oracle_stub_entry\n"
    "_oracle_stub_entry:\n"
    "    mov [_cpu + 16], esp\n"            /* cpu.r[ESP] */
    "    mov [_cpu + 4], ecx\n"             /* cpu.r[ECX] */
    "    mov [_cpu + 8], edx\n"             /* cpu.r[EDX] */
    "    mov edx, esp\n"
    "    mov esp, [_oracle_host_esp]\n"
    "    push edx\n"
    "    push eax\n"
    "    call _oracle_stub\n"
    "    add esp, 4\n"
    "    pop edx\n"
    "    mov ecx, [edx]\n"                  /* the return address */
    "    mov eax, [_cpu + 0]\n"
    "    mov edx, [_cpu + 8]\n"
    "    mov esp, [_cpu + 16]\n"            /* past the return address and the popped arguments */
    "    jmp ecx\n"
    ".att_syntax prefix\n");

void oracle_stub(uint32_t addr) {
    uint16_t cw;
    __asm__ volatile("fnstcw %0" : "=m"(cw));
    rt_last_call = addr;
    rt_stub_call(addr);
    __asm__ volatile("fldcw %0" : : "m"(cw));   /* the guest's x87 settings, whatever the host did */
}

/* ------------------------------------------------------------------ memory */
void rt_mem_init(void) {
    static int done;
    if (done) return;
    void *g = VirtualAlloc((void *)RT_GUEST_LOW, RT_GUEST_END - RT_GUEST_LOW, MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    if (g != (void *)RT_GUEST_LOW) rt_fatal("cannot reserve the guest map at %08x (in use in this process)", RT_GUEST_LOW);
    uint8_t *s = (uint8_t *)VirtualAlloc((void *)RT_STUB_BASE, 16 * STUB_SLOTS, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE);
    if (s != (uint8_t *)RT_STUB_BASE) rt_fatal("cannot map the stubs at %08x (the program must be large-address-aware)", RT_STUB_BASE);
    for (uint32_t n = 0; n < STUB_SLOTS; n++) {
        uint8_t *p = s + 16 * n;
        uint32_t a = RT_STUB_BASE + 16 * n, rel = (uint32_t)(uintptr_t)oracle_stub_entry - (a + 10);
        memset(p, 0xcc, 16);
        p[0] = 0xb8;                      /* mov eax, a */
        memcpy(p + 1, &a, 4);
        p[5] = 0xe9;                      /* jmp oracle_stub_entry */
        memcpy(p + 6, &rel, 4);
    }
    FlushInstructionCache(GetCurrentProcess(), s, 16 * STUB_SLOTS);
    M = 0;
    done = 1;
}

void rt_mem_commit(uint32_t addr, uint32_t size) {
    if (addr < RT_GUEST_LOW || (uint64_t)addr + size > RT_GUEST_END) rt_fatal("guest memory %08x+%x is outside the compact map", addr, size);
    uint32_t start = addr & ~0xfffu, end = (addr + size + 0xfff) & ~0xfffu;
    if (!VirtualAlloc((void *)start, end - start, MEM_COMMIT, PAGE_EXECUTE_READWRITE))
        rt_fatal("cannot commit guest memory %08x+%x", addr, size);
}

/* ------------------------------------------------------------------ calls into the guest */
void rt_dispatch_init(void) {
    uint16_t cw = 0x027f;                 /* as rt_load_image sets the recompiled engine's */
    __asm__ volatile("fldcw %0" : : "m"(cw));
}

/* Like rt.c's: the arguments go 64 bytes below the guest's ESP, so both sides use the same
 * guest stack addresses. */
uint32_t rt_guest_call(uint32_t addr, int nargs, const uint32_t *args) {
    uint32_t saved_esp = cpu.r[ESP], saved_host = oracle_host_esp;
    uint32_t esp = saved_esp - 64;
    for (int i = nargs - 1; i >= 0; i--) { esp -= 4; ST32(esp, args[i]); }
    uint32_t r = oracle_enter(addr, cpu.r[ECX], esp);
    oracle_host_esp = saved_host;
    cpu.r[ESP] = saved_esp;
    cpu.r[EAX] = r;
    return r;
}

uint32_t rt_guest_thiscall(uint32_t addr, uint32_t ecx, int nargs, const uint32_t *args) {
    uint32_t saved_ecx = cpu.r[ECX];
    cpu.r[ECX] = ecx;
    uint32_t r = rt_guest_call(addr, nargs, args);
    cpu.r[ECX] = saved_ecx;
    return r;
}
