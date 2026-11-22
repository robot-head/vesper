/* vwasm: the bootstrap seed's WebAssembly runner (SH6 lane 2, #1397).
 *
 * A small, portable interpreter for core WebAssembly, written in C11 with no
 * dependencies beyond the C standard library, so that any machine with a C
 * compiler can run the committed seed (bootstrap/seed/) with no Rust
 * toolchain. It runs what the Vesper wasm backend (src/wasm_backend and its
 * Vesper port) emits:
 *
 *   - the core MVP instruction set, with sign-extension, saturating
 *     truncation, bulk memory, multi-value blocks, reference instructions
 *     for funcref tables, and tail calls (return_call, return_call_indirect);
 *   - one memory, one funcref table, mutable globals;
 *   - a wasm32-wasip2 component: the runner takes its first core module
 *     (wit-component places the program's module first) and serves the
 *     WASI 0.2 functions the backend imports, lowered by the canonical ABI
 *     exactly as wit-component lowers them.
 *
 * Not supported (rejected at load with a message, never run wrongly): SIMD,
 * threads, memory64, multiple memories, exception handling and the WasmFX
 * stack-switching proposal (seed builds use the portable effect lowering).
 *
 * Usage:
 *   vwasm [options] FILE.wasm [ARG...]
 *     --invoke NAME   call the export NAME (default: "wasi:cli/run@0.2.0#run"
 *                     if present, else "vesper:program/entry@0.1.0#main",
 *                     else "_start", else "main")
 *     --print         print the flat results of the call, one per line,
 *                     as signed decimal (i32/i64) or %.17g (f32/f64)
 *     --fuel N        trap after N instructions (0 = unlimited, the default)
 *     --stack N       value stack slots (default 16M)
 *     --dump          print the module's imports and exports and exit
 *     --report FILE   write "ok", the flat results (one per line, as hex
 *                     bit patterns) and "memory PAGES FNV1A64" to FILE, or
 *                     "trap" if the call traps (used by the differential)
 *   ARG...            integer arguments for the invoked function
 *
 * Exit status: the invoked function's i32 result for wasi:cli/run (0 ok,
 * else 1); 0 otherwise; 2 for a usage or load error; 3 for a trap.
 *
 * Licence: MIT OR Apache-2.0, like the rest of the repository.
 */
#include <inttypes.h>
#include <math.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* ------------------------------------------------------------------ */
/* Errors                                                              */

static const char *report_path; /* --report FILE */

static _Noreturn void die(int code, const char *fmt, ...) {
  va_list ap;
  if (code == 3 && report_path) {
    FILE *r = fopen(report_path, "wb");
    if (r) {
      fputs("trap\n", r);
      fclose(r);
    }
  }
  fflush(stdout);
  va_start(ap, fmt);
  fputs("vwasm: ", stderr);
  vfprintf(stderr, fmt, ap);
  fputc('\n', stderr);
  va_end(ap);
  exit(code);
}
#define LOAD_FAIL(...) die(2, __VA_ARGS__)
#define TRAP(...) die(3, "trap: " __VA_ARGS__)

static void *xmalloc(size_t n) {
  void *p = calloc(n ? n : 1, 1);
  if (!p) die(2, "out of memory (%zu bytes)", n);
  return p;
}
static void *xrealloc(void *p, size_t n) {
  p = realloc(p, n ? n : 1);
  if (!p) die(2, "out of memory (%zu bytes)", n);
  return p;
}

/* ------------------------------------------------------------------ */
/* Binary reader                                                       */

typedef struct {
  const uint8_t *p, *end;
} Reader;

static uint8_t rd_u8(Reader *r) {
  if (r->p >= r->end) LOAD_FAIL("unexpected end of module");
  return *r->p++;
}
static uint64_t rd_uleb(Reader *r, int bits) {
  uint64_t v = 0;
  int shift = 0;
  for (;;) {
    uint8_t b = rd_u8(r);
    if (shift < 64) v |= (uint64_t)(b & 0x7f) << shift;
    shift += 7;
    if (!(b & 0x80)) break;
    if (shift > bits + 6) LOAD_FAIL("LEB128 too long");
  }
  return v;
}
static int64_t rd_sleb(Reader *r, int bits) {
  int64_t v = 0;
  int shift = 0;
  uint8_t b;
  do {
    b = rd_u8(r);
    if (shift < 64) v |= (int64_t)((uint64_t)(b & 0x7f) << shift);
    shift += 7;
    if (shift > bits + 6) LOAD_FAIL("LEB128 too long");
  } while (b & 0x80);
  if (shift < 64 && (b & 0x40)) v |= (int64_t)(~(uint64_t)0 << shift);
  return v;
}
static uint32_t rd_u32(Reader *r) { return (uint32_t)rd_uleb(r, 32); }

/* Fast in-body readers (the body was validated structurally at load). */
static inline uint32_t fu32(const uint8_t **pp) {
  const uint8_t *p = *pp;
  uint32_t v = 0;
  int shift = 0;
  uint8_t b;
  do {
    b = *p++;
    v |= (uint32_t)(b & 0x7f) << shift;
    shift += 7;
  } while (b & 0x80);
  *pp = p;
  return v;
}
static inline int64_t fs64(const uint8_t **pp) {
  const uint8_t *p = *pp;
  uint64_t v = 0;
  int shift = 0;
  uint8_t b;
  do {
    b = *p++;
    if (shift < 64) v |= (uint64_t)(b & 0x7f) << shift;
    shift += 7;
  } while (b & 0x80);
  if (shift < 64 && (b & 0x40)) v |= ~(uint64_t)0 << shift;
  *pp = p;
  return (int64_t)v;
}

/* ------------------------------------------------------------------ */
/* Module                                                              */

enum { T_I32 = 0x7f, T_I64 = 0x7e, T_F32 = 0x7d, T_F64 = 0x7c, T_FUNCREF = 0x70, T_EXTERNREF = 0x6f };

typedef struct {
  uint32_t np, nr;
  uint8_t *p, *r;
} FuncType;

struct Vm;
typedef void (*HostFn)(struct Vm *vm, uint64_t *args, uint64_t *results);

typedef struct {
  uint32_t type;
  HostFn host; /* imported */
  const char *imp_module, *imp_name;
  const uint8_t *body, *end; /* code after locals */
  uint32_t nlocals;          /* declared locals, excluding params */
  uint32_t body_off;         /* offset of body start in code section, for side table */
  uint32_t *side;            /* per-byte side table: block/loop/if -> end (and else) */
  uint32_t max_labels;
} Func;

typedef struct {
  char *name;
  uint8_t kind;
  uint32_t index;
} Export;

typedef struct {
  uint8_t type;
  uint64_t val;
} Global;

typedef struct {
  const uint8_t *data;
  uint32_t len;
  int dropped;
} Segment;

typedef struct Vm {
  FuncType *types;
  uint32_t ntypes;
  Func *funcs;
  uint32_t nfuncs, nimports;
  Global *globals;
  uint32_t nglobals;
  Export *exports;
  uint32_t nexports;
  uint32_t *table; /* function indices, UINT32_MAX = null */
  uint32_t table_len, table_max;
  int has_table;
  uint8_t *mem;
  uint64_t mem_pages, mem_max;
  int has_mem;
  Segment *datas;
  uint32_t ndatas;
  Segment *elems; /* element segments as function index arrays (data = u32 array) */
  uint32_t nelems;
  int32_t start;
  /* execution */
  uint64_t *stack;
  size_t stack_cap;
  uint64_t fuel; /* 0 = unlimited */
  int unlimited;
} Vm;

#define PAGE 65536u
#define NULL_FUNC UINT32_MAX

static int valtype_ok(uint8_t t) {
  return t == T_I32 || t == T_I64 || t == T_F32 || t == T_F64 || t == T_FUNCREF || t == T_EXTERNREF;
}

static char *rd_name(Reader *r) {
  uint32_t n = rd_u32(r);
  if ((size_t)(r->end - r->p) < n) LOAD_FAIL("name runs past the section");
  char *s = xmalloc(n + 1);
  memcpy(s, r->p, n);
  r->p += n;
  return s;
}

static void rd_limits(Reader *r, uint64_t *min, uint64_t *max, int *has_max) {
  uint8_t flag = rd_u8(r);
  if (flag & 0x04) LOAD_FAIL("memory64 is not supported");
  if (flag & 0x02) LOAD_FAIL("shared memory is not supported");
  *min = rd_u32(r);
  *has_max = flag & 1;
  *max = *has_max ? rd_u32(r) : 0;
}

/* Constant expressions: i32/i64/f32/f64.const, global.get, ref.null, ref.func. */
static uint64_t rd_const(Vm *vm, Reader *r, uint8_t *type_out) {
  uint8_t op = rd_u8(r);
  uint64_t v = 0;
  uint8_t t = 0;
  switch (op) {
  case 0x41: v = (uint32_t)(int32_t)rd_sleb(r, 32); t = T_I32; break;
  case 0x42: v = (uint64_t)rd_sleb(r, 64); t = T_I64; break;
  case 0x43: {
    uint32_t b;
    if (r->end - r->p < 4) LOAD_FAIL("truncated f32.const");
    memcpy(&b, r->p, 4);
    r->p += 4;
    v = b;
    t = T_F32;
    break;
  }
  case 0x44:
    if (r->end - r->p < 8) LOAD_FAIL("truncated f64.const");
    memcpy(&v, r->p, 8);
    r->p += 8;
    t = T_F64;
    break;
  case 0x23: {
    uint32_t g = rd_u32(r);
    if (g >= vm->nglobals) LOAD_FAIL("constant global.get out of range");
    v = vm->globals[g].val;
    t = vm->globals[g].type;
    break;
  }
  case 0xd0: t = rd_u8(r); v = NULL_FUNC; break;
  case 0xd2: v = rd_u32(r); t = T_FUNCREF; break;
  default: LOAD_FAIL("unsupported constant expression opcode 0x%02x", op);
  }
  if (rd_u8(r) != 0x0b) LOAD_FAIL("constant expression longer than one instruction");
  if (type_out) *type_out = t;
  return v;
}

/* Skip one instruction's immediates; used by the side-table pass. */
static void skip_blocktype(const uint8_t **pp) {
  uint8_t b = **pp;
  if (b == 0x40 || valtype_ok(b)) {
    (*pp)++;
    return;
  }
  (void)fs64(pp);
}

static void build_side_table(Vm *vm, Func *f) {
  size_t n = (size_t)(f->end - f->body);
  f->side = xmalloc(n * sizeof(uint32_t) + sizeof(uint32_t));
  uint32_t *open = xmalloc(sizeof(uint32_t) * (n + 1));
  size_t depth = 0, maxdepth = 0;
  const uint8_t *p = f->body;
  (void)vm;
  while (p < f->end) {
    uint32_t at = (uint32_t)(p - f->body);
    uint8_t op = *p++;
    switch (op) {
    case 0x02: case 0x03: case 0x04:
      skip_blocktype(&p);
      open[depth++] = at;
      if (depth > maxdepth) maxdepth = depth;
      f->side[at] = 0;
      break;
    case 0x05: /* else: remember on the if */
      if (!depth) LOAD_FAIL("else without if");
      f->side[at] = open[depth - 1];
      f->side[open[depth - 1]] = at; /* temporarily: if -> else */
      break;
    case 0x0b:
      if (depth) {
        uint32_t s = open[--depth];
        if (f->body[s] == 0x04 && f->side[s]) {
          uint32_t e = f->side[s];
          /* if: side[if] = end; side[if+1] cannot be used (blocktype byte), so
             store else position in the else slot and point else -> end. */
          f->side[e] = at;
          f->side[s] = at | 0x80000000u; /* flag: has else, else found by scan */
          /* Record else offset in a parallel slot: the byte after the if
             opcode is its blocktype, which is never an instruction start. */
          f->side[s + 1] = e;
        } else {
          f->side[s] = at;
        }
      } else if (p != f->end) {
        LOAD_FAIL("function body continues after its final end");
      }
      break;
    case 0x0c: case 0x0d: case 0x10: case 0x12: case 0x20: case 0x21: case 0x22: case 0x23: case 0x24:
    case 0x25: case 0x26: case 0xd2:
      (void)fu32(&p);
      break;
    case 0x0e: {
      uint32_t c = fu32(&p);
      for (uint32_t i = 0; i <= c; i++) (void)fu32(&p);
      break;
    }
    case 0x11: case 0x13:
      (void)fu32(&p);
      (void)fu32(&p);
      break;
    case 0x1c: {
      uint32_t c = fu32(&p);
      p += c;
      break;
    }
    case 0x41: case 0x42: (void)fs64(&p); break;
    case 0x43: p += 4; break;
    case 0x44: p += 8; break;
    case 0xd0: p++; break;
    case 0xfc: {
      uint32_t sub = fu32(&p);
      switch (sub) {
      case 8: (void)fu32(&p); p++; break;          /* memory.init seg, mem */
      case 9: (void)fu32(&p); break;               /* data.drop */
      case 10: p += 2; break;                      /* memory.copy 0 0 */
      case 11: p++; break;                         /* memory.fill 0 */
      case 12: (void)fu32(&p); (void)fu32(&p); break; /* table.init */
      case 13: (void)fu32(&p); break;              /* elem.drop */
      case 14: (void)fu32(&p); (void)fu32(&p); break; /* table.copy */
      case 15: case 16: case 17: (void)fu32(&p); break; /* table.grow/size/fill */
      default:
        if (sub > 7) LOAD_FAIL("unsupported 0xfc instruction %u", sub);
      }
      break;
    }
    case 0xe0: case 0xe1: case 0xe2: case 0xe3: case 0xe4: case 0xe5: case 0xe6:
      LOAD_FAIL("the stack-switching proposal is not supported (build the seed with portable effects)");
    case 0xfd: LOAD_FAIL("SIMD is not supported");
    case 0xfe: LOAD_FAIL("threads are not supported");
    case 0x06: case 0x07: case 0x08: case 0x09: case 0x0a: case 0x18: case 0x19: case 0x1f:
      LOAD_FAIL("exception handling is not supported");
    default:
      if (op >= 0x28 && op <= 0x3e) {
        (void)fu32(&p);
        (void)fu32(&p);
      } else if (op == 0x3f || op == 0x40) {
        p++;
      } else if (op == 0x00 || op == 0x01 || op == 0x0f || op == 0x1a || op == 0x1b || (op >= 0x45 && op <= 0xc4) ||
                 op == 0xd1) {
        /* no immediates */
      } else {
        LOAD_FAIL("unsupported opcode 0x%02x", op);
      }
    }
    if (p > f->end) LOAD_FAIL("instruction runs past the function body");
  }
  if (depth) LOAD_FAIL("unterminated block in function body");
  f->max_labels = (uint32_t)maxdepth + 1;
  free(open);
}

static const uint8_t *find_core_module(const uint8_t *buf, size_t len, size_t *mlen) {
  if (len < 8 || memcmp(buf, "\0asm", 4) != 0) LOAD_FAIL("not a WebAssembly binary");
  if (buf[4] == 1 && buf[5] == 0 && buf[6] == 0 && buf[7] == 0) {
    *mlen = len;
    return buf;
  }
  if (!(buf[6] == 1 && buf[7] == 0)) LOAD_FAIL("unknown WebAssembly binary version/layer");
  /* Component: the first core module section (id 1). */
  Reader r = {buf + 8, buf + len};
  while (r.p < r.end) {
    uint8_t id = rd_u8(&r);
    uint32_t size = rd_u32(&r);
    if ((size_t)(r.end - r.p) < size) LOAD_FAIL("component section runs past the file");
    if (id == 1) {
      *mlen = size;
      return r.p;
    }
    r.p += size;
  }
  LOAD_FAIL("component holds no core module");
  return NULL;
}

static HostFn resolve_host(const char *module, const char *name);

static void load(Vm *vm, const uint8_t *buf, size_t len) {
  if (len < 8 || memcmp(buf, "\0asm\1\0\0\0", 8) != 0) LOAD_FAIL("not a core WebAssembly module");
  Reader r = {buf + 8, buf + len};
  uint32_t *func_types = NULL; /* defined functions' types */
  uint32_t ndefined = 0;
  vm->start = -1;
  while (r.p < r.end) {
    uint8_t id = rd_u8(&r);
    uint32_t size = rd_u32(&r);
    if ((size_t)(r.end - r.p) < size) LOAD_FAIL("section runs past the module");
    Reader s = {r.p, r.p + size};
    r.p += size;
    switch (id) {
    case 0: break; /* custom */
    case 1: {      /* type */
      uint32_t n = rd_u32(&s);
      vm->types = xmalloc(sizeof(FuncType) * n);
      vm->ntypes = n;
      for (uint32_t i = 0; i < n; i++) {
        uint8_t form = rd_u8(&s);
        if (form == 0x4e || form == 0x4f || form == 0x50) LOAD_FAIL("GC/recursive types are not supported");
        if (form == 0x5d) LOAD_FAIL("continuation types are not supported (portable effects only)");
        if (form != 0x60) LOAD_FAIL("unsupported type form 0x%02x", form);
        FuncType *t = &vm->types[i];
        t->np = rd_u32(&s);
        t->p = xmalloc(t->np);
        for (uint32_t k = 0; k < t->np; k++) {
          t->p[k] = rd_u8(&s);
          if (!valtype_ok(t->p[k])) LOAD_FAIL("unsupported value type 0x%02x", t->p[k]);
        }
        t->nr = rd_u32(&s);
        t->r = xmalloc(t->nr);
        for (uint32_t k = 0; k < t->nr; k++) {
          t->r[k] = rd_u8(&s);
          if (!valtype_ok(t->r[k])) LOAD_FAIL("unsupported value type 0x%02x", t->r[k]);
        }
      }
      break;
    }
    case 2: { /* import */
      uint32_t n = rd_u32(&s);
      for (uint32_t i = 0; i < n; i++) {
        char *m = rd_name(&s), *nm = rd_name(&s);
        uint8_t kind = rd_u8(&s);
        if (kind == 0) {
          uint32_t ty = rd_u32(&s);
          if (ty >= vm->ntypes) LOAD_FAIL("import type out of range");
          vm->funcs = xrealloc(vm->funcs, sizeof(Func) * (vm->nfuncs + 1));
          Func *f = &vm->funcs[vm->nfuncs++];
          memset(f, 0, sizeof *f);
          f->type = ty;
          f->imp_module = m;
          f->imp_name = nm;
          f->host = resolve_host(m, nm);
          vm->nimports++;
        } else {
          LOAD_FAIL("import %s.%s: only function imports are supported", m, nm);
        }
      }
      break;
    }
    case 3: { /* function */
      ndefined = rd_u32(&s);
      func_types = xmalloc(sizeof(uint32_t) * ndefined);
      for (uint32_t i = 0; i < ndefined; i++) {
        func_types[i] = rd_u32(&s);
        if (func_types[i] >= vm->ntypes) LOAD_FAIL("function type out of range");
      }
      break;
    }
    case 4: { /* table */
      uint32_t n = rd_u32(&s);
      if (n > 1) LOAD_FAIL("multiple tables are not supported");
      if (n) {
        uint8_t et = rd_u8(&s);
        if (et != T_FUNCREF) LOAD_FAIL("only funcref tables are supported");
        uint64_t mn, mx;
        int hm;
        rd_limits(&s, &mn, &mx, &hm);
        vm->has_table = 1;
        vm->table_len = (uint32_t)mn;
        vm->table_max = hm ? (uint32_t)mx : UINT32_MAX;
        vm->table = xmalloc(sizeof(uint32_t) * (vm->table_len + 1));
        for (uint32_t k = 0; k < vm->table_len; k++) vm->table[k] = NULL_FUNC;
      }
      break;
    }
    case 5: { /* memory */
      uint32_t n = rd_u32(&s);
      if (n > 1) LOAD_FAIL("multiple memories are not supported");
      if (n) {
        uint64_t mn, mx;
        int hm;
        rd_limits(&s, &mn, &mx, &hm);
        vm->has_mem = 1;
        vm->mem_pages = mn;
        vm->mem_max = hm ? mx : 65536;
        if (vm->mem_max > 65536) vm->mem_max = 65536;
        vm->mem = xmalloc((size_t)(mn * PAGE));
      }
      break;
    }
    case 13: LOAD_FAIL("exception tags are not supported");
    case 6: { /* global */
      uint32_t n = rd_u32(&s);
      vm->globals = xmalloc(sizeof(Global) * (n + 1));
      for (uint32_t i = 0; i < n; i++) {
        uint8_t t = rd_u8(&s);
        (void)rd_u8(&s); /* mutability */
        vm->globals[vm->nglobals].type = t;
        uint64_t v = rd_const(vm, &s, NULL);
        vm->globals[vm->nglobals++].val = v;
      }
      break;
    }
    case 7: { /* export */
      uint32_t n = rd_u32(&s);
      vm->exports = xmalloc(sizeof(Export) * (n + 1));
      for (uint32_t i = 0; i < n; i++) {
        Export *e = &vm->exports[vm->nexports++];
        e->name = rd_name(&s);
        e->kind = rd_u8(&s);
        e->index = rd_u32(&s);
      }
      break;
    }
    case 8: vm->start = (int32_t)rd_u32(&s); break;
    case 9: { /* element */
      uint32_t n = rd_u32(&s);
      vm->elems = xmalloc(sizeof(Segment) * (n + 1));
      for (uint32_t i = 0; i < n; i++) {
        uint32_t flags = rd_u32(&s);
        int passive = flags & 1, explicit_table = flags & 2, exprs = flags & 4;
        int declarative = passive && explicit_table;
        uint64_t offset = 0;
        if (!passive) {
          if (explicit_table && rd_u32(&s) != 0) LOAD_FAIL("element segment for a second table");
          offset = rd_const(vm, &s, NULL);
        }
        if (passive || explicit_table) {
          uint8_t k = rd_u8(&s); /* elemkind 0x00 or reftype */
          (void)k;
        }
        uint32_t cnt = rd_u32(&s);
        uint32_t *items = xmalloc(sizeof(uint32_t) * (cnt + 1));
        for (uint32_t k = 0; k < cnt; k++) items[k] = exprs ? (uint32_t)rd_const(vm, &s, NULL) : rd_u32(&s);
        Segment *seg = &vm->elems[vm->nelems++];
        seg->data = (const uint8_t *)items;
        seg->len = cnt;
        seg->dropped = declarative;
        if (!passive) {
          if (!vm->has_table || offset + cnt > vm->table_len) LOAD_FAIL("element segment out of table bounds");
          memcpy(vm->table + offset, items, sizeof(uint32_t) * cnt);
          seg->dropped = 1;
        }
      }
      break;
    }
    case 12: (void)rd_u32(&s); break; /* data count */
    case 10: { /* code */
      uint32_t n = rd_u32(&s);
      if (n != ndefined) LOAD_FAIL("function and code section counts differ");
      vm->funcs = xrealloc(vm->funcs, sizeof(Func) * (vm->nfuncs + n + 1));
      for (uint32_t i = 0; i < n; i++) {
        uint32_t fsize = rd_u32(&s);
        if ((size_t)(s.end - s.p) < fsize) LOAD_FAIL("function body runs past the section");
        Reader b = {s.p, s.p + fsize};
        s.p += fsize;
        Func *f = &vm->funcs[vm->nfuncs++];
        memset(f, 0, sizeof *f);
        f->type = func_types[i];
        uint32_t groups = rd_u32(&b);
        uint64_t total = 0;
        for (uint32_t g = 0; g < groups; g++) {
          uint32_t c = rd_u32(&b);
          uint8_t t = rd_u8(&b);
          if (!valtype_ok(t)) LOAD_FAIL("unsupported local type 0x%02x", t);
          total += c;
          if (total > 50000) LOAD_FAIL("too many locals");
        }
        f->nlocals = (uint32_t)total;
        f->body = b.p;
        f->end = b.end;
        if (f->end == f->body || f->end[-1] != 0x0b) LOAD_FAIL("function body does not end with end");
        build_side_table(vm, f);
      }
      break;
    }
    case 11: { /* data */
      uint32_t n = rd_u32(&s);
      vm->datas = xmalloc(sizeof(Segment) * (n + 1));
      for (uint32_t i = 0; i < n; i++) {
        uint32_t flags = rd_u32(&s);
        uint64_t offset = 0;
        int passive = flags == 1;
        if (flags == 2 && rd_u32(&s) != 0) LOAD_FAIL("data segment for a second memory");
        if (!passive) offset = (uint32_t)rd_const(vm, &s, NULL);
        uint32_t dl = rd_u32(&s);
        if ((size_t)(s.end - s.p) < dl) LOAD_FAIL("data segment runs past the section");
        Segment *seg = &vm->datas[vm->ndatas++];
        seg->data = s.p;
        seg->len = dl;
        s.p += dl;
        if (!passive) {
          if (!vm->has_mem || offset + dl > vm->mem_pages * PAGE) LOAD_FAIL("data segment out of memory bounds");
          memcpy(vm->mem + offset, seg->data, dl);
          seg->dropped = 1;
        }
      }
      break;
    }
    default: LOAD_FAIL("unsupported section id %u", id);
    }
  }
  if (vm->nfuncs != vm->nimports + ndefined) LOAD_FAIL("module has functions without code");
  free(func_types);
}

/* ------------------------------------------------------------------ */
/* Host functions: the WASI 0.2 subset the Vesper wasm backend imports  */

static uint8_t *mem_at(Vm *vm, uint64_t addr, uint64_t n) {
  if (!vm->has_mem || addr + n > vm->mem_pages * PAGE || addr + n < addr) TRAP("out of bounds memory access");
  return vm->mem + addr;
}

static void host_get_stdout(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)vm;
  (void)a;
  r[0] = 1; /* resource handle */
}
/* [method]output-stream.blocking-write-and-flush(self, ptr, len, retptr):
   result<_, stream-error> is stored at retptr as a u8 discriminant. */
static void host_write_flush(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  uint32_t ptr = (uint32_t)a[1], len = (uint32_t)a[2], ret = (uint32_t)a[3];
  uint8_t *src = mem_at(vm, ptr, len);
  if (len > 4096) TRAP("blocking-write-and-flush of more than 4096 bytes");
  int ok = fwrite(src, 1, len, stdout) == len && fflush(stdout) == 0;
  uint8_t *out = mem_at(vm, ret, 12);
  memset(out, 0, 12);
  out[0] = ok ? 0 : 1;
  if (!ok) out[4] = 1; /* stream-error::closed */
}
static void host_nop(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)vm;
  (void)a;
  (void)r;
}
static void host_now(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)vm;
  (void)a;
  struct timespec ts;
  if (timespec_get(&ts, TIME_UTC) != TIME_UTC) TRAP("clock unavailable");
  r[0] = (uint64_t)ts.tv_sec * 1000000000u + (uint64_t)ts.tv_nsec;
}
/* Beacon trace: no collector is attached to a seed run. current-ctx writes a
   zero span context (32 bytes); start-span/start-child write a zero active
   span (56 bytes) at their retptr, the last argument. */
static void host_trace_ctx(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  memset(mem_at(vm, (uint32_t)a[0], 32), 0, 32);
}
static void host_trace_span(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  memset(mem_at(vm, (uint32_t)a[3], 56), 0, 56);
}
static void host_trace_child(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  memset(mem_at(vm, (uint32_t)a[7], 56), 0, 56);
}
static void host_missing(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)vm;
  (void)a;
  (void)r;
  TRAP("called an import this runner does not provide");
}

static HostFn resolve_host(const char *module, const char *name) {
  static const struct {
    const char *m, *n;
    HostFn f;
  } table[] = {
      {"wasi:cli/stdout@0.2.0", "get-stdout", host_get_stdout},
      {"wasi:io/streams@0.2.0", "[method]output-stream.blocking-write-and-flush", host_write_flush},
      {"wasi:io/streams@0.2.0", "[resource-drop]output-stream", host_nop},
      {"wasi:clocks/monotonic-clock@0.2.0", "now", host_now},
      {"vesper:beacon/trace@0.1.0", "current-ctx", host_trace_ctx},
      {"vesper:beacon/trace@0.1.0", "start-span", host_trace_span},
      {"vesper:beacon/trace@0.1.0", "start-child", host_trace_child},
      {"vesper:beacon/trace@0.1.0", "emit-ok", host_nop},
      {"vesper:beacon/trace@0.1.0", "emit-err", host_nop},
      {"vesper:beacon/trace@0.1.0", "emit-attr", host_nop},
  };
  for (size_t i = 0; i < sizeof table / sizeof table[0]; i++)
    if (!strcmp(table[i].m, module) && !strcmp(table[i].n, name)) return table[i].f;
  return host_missing;
}

/* ------------------------------------------------------------------ */
/* Numeric helpers with WebAssembly semantics                          */

static inline float f32_of(uint64_t v) {
  uint32_t b = (uint32_t)v;
  float f;
  memcpy(&f, &b, 4);
  return f;
}
static inline uint64_t of_f32(float f) {
  uint32_t b;
  memcpy(&b, &f, 4);
  return b;
}
static inline double f64_of(uint64_t v) {
  double d;
  memcpy(&d, &v, 8);
  return d;
}
static inline uint64_t of_f64(double d) {
  uint64_t b;
  memcpy(&b, &d, 8);
  return b;
}
/* Canonical NaN results are allowed to be any NaN; keep the host's. */
static double wmin(double a, double b) {
  if (isnan(a) || isnan(b)) return NAN;
  if (a == 0 && b == 0) return signbit(a) ? a : b;
  return a < b ? a : b;
}
static double wmax(double a, double b) {
  if (isnan(a) || isnan(b)) return NAN;
  if (a == 0 && b == 0) return signbit(a) ? b : a;
  return a > b ? a : b;
}
static float wminf(float a, float b) {
  if (isnan(a) || isnan(b)) return NAN;
  if (a == 0 && b == 0) return signbit(a) ? a : b;
  return a < b ? a : b;
}
static float wmaxf(float a, float b) {
  if (isnan(a) || isnan(b)) return NAN;
  if (a == 0 && b == 0) return signbit(a) ? b : a;
  return a > b ? a : b;
}
static double wnearest(double x) {
  if (isnan(x) || isinf(x) || x == 0) return x;
  double r = nearbyint(x); /* default rounding mode is to-nearest-even */
  if (r == 0) return copysign(0.0, x);
  return r;
}
static float wnearestf(float x) {
  if (isnan(x) || isinf(x) || x == 0) return x;
  float r = nearbyintf(x);
  if (r == 0) return copysignf(0.0f, x);
  return r;
}

/* Truncation: trapping (sat = 0) or saturating (sat = 1). */
static int64_t trunc_s(double x, int bits, int sat) {
  double lo = bits == 32 ? -2147483648.0 : -9223372036854775808.0;
  double hi = bits == 32 ? 2147483648.0 : 9223372036854775808.0;
  if (isnan(x)) {
    if (sat) return 0;
    TRAP("invalid conversion to integer");
  }
  x = trunc(x);
  if (x < lo || x >= hi) {
    if (!sat) TRAP("integer overflow");
    return x < lo ? (bits == 32 ? INT32_MIN : INT64_MIN) : (bits == 32 ? INT32_MAX : INT64_MAX);
  }
  return (int64_t)x;
}
static uint64_t trunc_u(double x, int bits, int sat) {
  double hi = bits == 32 ? 4294967296.0 : 18446744073709551616.0;
  if (isnan(x)) {
    if (sat) return 0;
    TRAP("invalid conversion to integer");
  }
  x = trunc(x);
  if (x <= -1.0 || x >= hi) {
    if (!sat) TRAP("integer overflow");
    return x <= -1.0 ? 0 : (bits == 32 ? UINT32_MAX : UINT64_MAX);
  }
  if (x < 0) return 0;
  return (uint64_t)x;
}
static uint32_t clz32(uint32_t x) {
  uint32_t n = 0;
  if (!x) return 32;
  while (!(x & 0x80000000u)) {
    x <<= 1;
    n++;
  }
  return n;
}
static uint32_t ctz32(uint32_t x) {
  uint32_t n = 0;
  if (!x) return 32;
  while (!(x & 1)) {
    x >>= 1;
    n++;
  }
  return n;
}
static uint32_t pop32(uint32_t x) {
  uint32_t n = 0;
  while (x) {
    n += x & 1;
    x >>= 1;
  }
  return n;
}
static uint64_t clz64(uint64_t x) {
  uint64_t n = 0;
  if (!x) return 64;
  while (!(x & 0x8000000000000000ull)) {
    x <<= 1;
    n++;
  }
  return n;
}
static uint64_t ctz64(uint64_t x) {
  uint64_t n = 0;
  if (!x) return 64;
  while (!(x & 1)) {
    x >>= 1;
    n++;
  }
  return n;
}
static uint64_t pop64(uint64_t x) {
  uint64_t n = 0;
  while (x) {
    n += x & 1;
    x >>= 1;
  }
  return n;
}
static inline uint32_t rotl32(uint32_t x, uint32_t k) {
  k &= 31;
  return k ? (x << k) | (x >> (32 - k)) : x;
}
static inline uint32_t rotr32(uint32_t x, uint32_t k) {
  k &= 31;
  return k ? (x >> k) | (x << (32 - k)) : x;
}
static inline uint64_t rotl64(uint64_t x, uint64_t k) {
  k &= 63;
  return k ? (x << k) | (x >> (64 - k)) : x;
}
static inline uint64_t rotr64(uint64_t x, uint64_t k) {
  k &= 63;
  return k ? (x >> k) | (x << (64 - k)) : x;
}

/* Little-endian memory access, independent of the host's byte order. */
static inline uint64_t ld(const uint8_t *p, int n) {
  uint64_t v = 0;
  for (int i = n - 1; i >= 0; i--) v = (v << 8) | p[i];
  return v;
}
static inline void st(uint8_t *p, uint64_t v, int n) {
  for (int i = 0; i < n; i++) {
    p[i] = (uint8_t)v;
    v >>= 8;
  }
}

/* ------------------------------------------------------------------ */
/* Interpreter                                                         */

typedef struct {
  uint32_t cont;   /* offset to continue at on branch (loop start or end) */
  uint32_t height; /* value stack height at block entry (below params) */
  uint32_t arity;  /* values carried by a branch */
  uint32_t nres;   /* values left at the block's end */
} Label;

typedef struct {
  uint32_t func;
  const uint8_t *pc;
  size_t locals; /* stack index of local 0 */
  uint32_t label_base;
} Frame;

static void blocktype(Vm *vm, const uint8_t **pp, uint32_t *np, uint32_t *nr) {
  uint8_t b = **pp;
  if (b == 0x40) {
    (*pp)++;
    *np = 0;
    *nr = 0;
  } else if (valtype_ok(b)) {
    (*pp)++;
    *np = 0;
    *nr = 1;
  } else {
    int64_t t = fs64(pp);
    if (t < 0 || (uint64_t)t >= vm->ntypes) TRAP("block type out of range");
    *np = vm->types[t].np;
    *nr = vm->types[t].nr;
  }
}

static int types_equal(const FuncType *a, const FuncType *b) {
  return a->np == b->np && a->nr == b->nr && !memcmp(a->p, b->p, a->np) && !memcmp(a->r, b->r, a->nr);
}

#define MAX_FRAMES 200000

static void invoke(Vm *vm, uint32_t entry, uint64_t *args, uint64_t *results) {
  Frame *frames = xmalloc(sizeof(Frame) * MAX_FRAMES);
  size_t label_cap = 1 << 16;
  Label *labels = xmalloc(sizeof(Label) * label_cap);
  uint64_t *S = vm->stack;
  size_t sp = 0, cap = vm->stack_cap;
  uint32_t nframes = 0, nlabels = 0;
  const FuncType *et = &vm->types[vm->funcs[entry].type];
  for (uint32_t i = 0; i < et->np; i++) S[sp++] = args[i];
  if (vm->funcs[entry].host) {
    vm->funcs[entry].host(vm, S, S);
    memcpy(results, S, sizeof(uint64_t) * et->nr);
    free(frames);
    free(labels);
    return;
  }
  Func *F;
  const uint8_t *pc, *base;
  size_t L;
  uint32_t fidx, callee, depth;
  int tail;

#define ENTER(idx)                                                                   \
  do {                                                                               \
    fidx = (idx);                                                                    \
    F = &vm->funcs[fidx];                                                            \
    const FuncType *ft_ = &vm->types[F->type];                                       \
    if (nframes >= MAX_FRAMES) TRAP("call stack exhausted");                         \
    L = sp - ft_->np;                                                                \
    if (sp + F->nlocals + 1024 > cap) TRAP("value stack exhausted");                  \
    memset(S + sp, 0, sizeof(uint64_t) * F->nlocals);                                \
    sp += F->nlocals;                                                                \
    frames[nframes].func = fidx;                                                     \
    frames[nframes].locals = L;                                                      \
    frames[nframes].label_base = nlabels;                                            \
    nframes++;                                                                       \
    if (nlabels + F->max_labels + 1 >= label_cap) {                                  \
      label_cap = (nlabels + F->max_labels + 1) * 2;                                 \
      labels = xrealloc(labels, sizeof(Label) * label_cap);                          \
    }                                                                                \
    base = F->body;                                                                  \
    pc = F->body;                                                                    \
    labels[nlabels].cont = (uint32_t)(F->end - F->body) - 1;                         \
    labels[nlabels].height = 0;                                                      \
    labels[nlabels].arity = ft_->nr;                                                 \
    labels[nlabels].nres = ft_->nr;                                                  \
    nlabels++;                                                                       \
  } while (0)

  ENTER(entry);

#define POP() (S[--sp])
#define PUSH(v) (S[sp++] = (v))
#define TOP() (S[sp - 1])
#define MEMOP(n)                                                                     \
  uint32_t align_ = fu32(&pc);                                                       \
  (void)align_;                                                                      \
  uint64_t ea_ = fu32(&pc);
#define ADDR(n)                                                                      \
  ea_ += (uint32_t)POP();                                                            \
  if (ea_ + (n) > vm->mem_pages * PAGE) TRAP("out of bounds memory access");
#define BIN32(expr)                                                                  \
  {                                                                                  \
    uint32_t b = (uint32_t)POP(), a = (uint32_t)POP();                               \
    PUSH((uint32_t)(expr));                                                          \
  }                                                                                  \
  break;
#define BIN64(expr)                                                                  \
  {                                                                                  \
    uint64_t b = POP(), a = POP();                                                   \
    PUSH((uint64_t)(expr));                                                          \
  }                                                                                  \
  break;
#define CMP32(expr)                                                                  \
  {                                                                                  \
    uint32_t b = (uint32_t)POP(), a = (uint32_t)POP();                               \
    PUSH((expr) ? 1u : 0u);                                                          \
  }                                                                                  \
  break;
#define CMP64(expr)                                                                  \
  {                                                                                  \
    uint64_t b = POP(), a = POP();                                                   \
    PUSH((expr) ? 1u : 0u);                                                          \
  }                                                                                  \
  break;
#define FBIN32(expr)                                                                 \
  {                                                                                  \
    float b = f32_of(POP()), a = f32_of(POP());                                      \
    PUSH(of_f32(expr));                                                              \
  }                                                                                  \
  break;
#define FBIN64(expr)                                                                 \
  {                                                                                  \
    double b = f64_of(POP()), a = f64_of(POP());                                     \
    PUSH(of_f64(expr));                                                              \
  }                                                                                  \
  break;
#define FCMP32(expr)                                                                 \
  {                                                                                  \
    float b = f32_of(POP()), a = f32_of(POP());                                      \
    PUSH((expr) ? 1u : 0u);                                                          \
  }                                                                                  \
  break;
#define FCMP64(expr)                                                                 \
  {                                                                                  \
    double b = f64_of(POP()), a = f64_of(POP());                                     \
    PUSH((expr) ? 1u : 0u);                                                          \
  }                                                                                  \
  break;
#define FUN32(expr)                                                                  \
  {                                                                                  \
    float a = f32_of(POP());                                                         \
    PUSH(of_f32(expr));                                                              \
  }                                                                                  \
  break;
#define FUN64(expr)                                                                  \
  {                                                                                  \
    double a = f64_of(POP());                                                        \
    PUSH(of_f64(expr));                                                              \
  }                                                                                  \
  break;

  for (;;) {
    if (!vm->unlimited) {
      if (vm->fuel == 0) TRAP("fuel exhausted");
      vm->fuel--;
    }
    uint32_t here = (uint32_t)(pc - base);
    uint8_t op = *pc++;
    switch (op) {
    case 0x00: TRAP("unreachable");
    case 0x01: break;
    case 0x02: { /* block */
      uint32_t np, nr;
      blocktype(vm, &pc, &np, &nr);
      Label *l = &labels[nlabels++];
      l->cont = F->side[here];
      l->height = (uint32_t)(sp - L) - np;
      l->arity = nr;
      l->nres = nr;
      break;
    }
    case 0x03: { /* loop */
      uint32_t np, nr;
      blocktype(vm, &pc, &np, &nr);
      Label *l = &labels[nlabels++];
      l->cont = here; /* branch re-enters the loop instruction */
      l->height = (uint32_t)(sp - L) - np;
      l->arity = np;
      l->nres = nr;
      /* Mark as loop: cont points at the loop opcode itself. */
      break;
    }
    case 0x04: { /* if */
      uint32_t np, nr;
      uint32_t side = F->side[here];
      uint32_t else_at = (side & 0x80000000u) ? F->side[here + 1] : 0;
      uint32_t end_at = side & 0x7fffffffu;
      blocktype(vm, &pc, &np, &nr);
      uint32_t c = (uint32_t)POP();
      if (c) {
        Label *l = &labels[nlabels++];
        l->cont = end_at;
        l->height = (uint32_t)(sp - L) - np;
        l->arity = nr;
        l->nres = nr;
      } else if (else_at) {
        Label *l = &labels[nlabels++];
        l->cont = end_at;
        l->height = (uint32_t)(sp - L) - np;
        l->arity = nr;
        l->nres = nr;
        pc = base + else_at + 1;
      } else {
        pc = base + end_at + 1; /* no else: params pass through as results */
      }
      break;
    }
    case 0x05: /* else reached from the then-arm: jump to end */
      pc = base + F->side[here];
      break;
    case 0x0b: { /* end */
      Label *l = &labels[nlabels - 1];
      nlabels--;
      if (nlabels == frames[nframes - 1].label_base) {
        /* function end: return */
        goto do_return;
      }
      (void)l;
      break;
    }
    case 0x0c: { /* br */
      depth = fu32(&pc);
    do_branch:;
      Label *l = &labels[nlabels - 1 - depth];
      uint32_t arity = l->arity;
      size_t dst = L + l->height;
      if (dst != sp - arity) memmove(S + dst, S + sp - arity, sizeof(uint64_t) * arity);
      sp = dst + arity;
      if (nlabels - 1 - depth == frames[nframes - 1].label_base) goto do_return_values;
      if (base[l->cont] == 0x03) {
        /* loop: keep the label, restart after the loop's blocktype */
        nlabels -= depth;
        pc = base + l->cont + 1;
        uint32_t np, nr;
        blocktype(vm, &pc, &np, &nr);
      } else {
        nlabels -= depth + 1;
        pc = base + l->cont + 1;
      }
      break;
    }
    case 0x0d: { /* br_if */
      depth = fu32(&pc);
      if ((uint32_t)POP()) goto do_branch;
      break;
    }
    case 0x0e: { /* br_table */
      uint32_t n = fu32(&pc);
      uint32_t idx = (uint32_t)POP();
      depth = 0;
      for (uint32_t i = 0; i <= n; i++) {
        uint32_t d = fu32(&pc);
        if (i == idx || i == n) {
          depth = d;
          if (i == idx) break;
        }
      }
      goto do_branch;
    }
    case 0x0f: /* return */
    do_return : {
      const FuncType *ft = &vm->types[F->type];
      uint32_t nr = ft->nr;
      memmove(S + L, S + sp - nr, sizeof(uint64_t) * nr);
      sp = L + nr;
    }
    do_return_values : {
      nlabels = frames[nframes - 1].label_base;
      nframes--;
      if (nframes == 0) {
        const FuncType *ft = &vm->types[vm->funcs[entry].type];
        memcpy(results, S, sizeof(uint64_t) * ft->nr);
        free(frames);
        free(labels);
        return;
      }
      Frame *fr = &frames[nframes - 1];
      fidx = fr->func;
      F = &vm->funcs[fidx];
      base = F->body;
      pc = fr->pc;
      L = fr->locals;
      break;
    }
    case 0x10: case 0x12: /* call, return_call */
      callee = fu32(&pc);
      tail = op == 0x12;
      goto do_call;
    case 0x11: case 0x13: { /* call_indirect, return_call_indirect */
      uint32_t ty = fu32(&pc);
      (void)fu32(&pc); /* table 0 */
      tail = op == 0x13;
      uint32_t i = (uint32_t)POP();
      if (!vm->has_table || i >= vm->table_len) TRAP("undefined element");
      callee = vm->table[i];
      if (callee == NULL_FUNC) TRAP("uninitialized element");
      if (!types_equal(&vm->types[ty], &vm->types[vm->funcs[callee].type])) TRAP("indirect call type mismatch");
    }
    do_call : {
      if (callee >= vm->nfuncs) TRAP("call target out of range");
      Func *g = &vm->funcs[callee];
      const FuncType *gt = &vm->types[g->type];
      if (g->host) {
        uint64_t tmp[64], res[64];
        if (gt->np > 64 || gt->nr > 64) TRAP("host call with too many values");
        sp -= gt->np;
        memcpy(tmp, S + sp, sizeof(uint64_t) * gt->np);
        g->host(vm, tmp, res);
        memcpy(S + sp, res, sizeof(uint64_t) * gt->nr);
        sp += gt->nr;
        if (tail) goto do_return;
        break;
      }
      if (tail) {
        /* replace the current frame */
        memmove(S + L, S + sp - gt->np, sizeof(uint64_t) * gt->np);
        sp = L + gt->np;
        nlabels = frames[nframes - 1].label_base;
        nframes--;
      } else {
        frames[nframes - 1].pc = pc;
      }
      ENTER(callee);
      break;
    }
    case 0x1a: sp--; break;
    case 0x1b: { /* select */
      uint32_t c = (uint32_t)POP();
      uint64_t b = POP(), a = POP();
      PUSH(c ? a : b);
      break;
    }
    case 0x1c: { /* typed select */
      uint32_t n = fu32(&pc);
      pc += n;
      uint32_t c = (uint32_t)POP();
      uint64_t b = POP(), a = POP();
      PUSH(c ? a : b);
      break;
    }
    case 0x20: { uint32_t i = fu32(&pc); PUSH(S[L + i]); break; }
    case 0x21: { uint32_t i = fu32(&pc); S[L + i] = POP(); break; }
    case 0x22: { uint32_t i = fu32(&pc); S[L + i] = TOP(); break; }
    case 0x23: { uint32_t i = fu32(&pc); PUSH(vm->globals[i].val); break; }
    case 0x24: { uint32_t i = fu32(&pc); vm->globals[i].val = POP(); break; }
    case 0x25: { /* table.get */
      (void)fu32(&pc);
      uint32_t i = (uint32_t)POP();
      if (i >= vm->table_len) TRAP("out of bounds table access");
      PUSH(vm->table[i]);
      break;
    }
    case 0x26: { /* table.set */
      (void)fu32(&pc);
      uint32_t v = (uint32_t)POP(), i = (uint32_t)POP();
      if (i >= vm->table_len) TRAP("out of bounds table access");
      vm->table[i] = v;
      break;
    }
    /* loads */
    case 0x28: { MEMOP(4) ADDR(4) PUSH((uint32_t)ld(vm->mem + ea_, 4)); break; }
    case 0x29: { MEMOP(8) ADDR(8) PUSH(ld(vm->mem + ea_, 8)); break; }
    case 0x2a: { MEMOP(4) ADDR(4) PUSH((uint32_t)ld(vm->mem + ea_, 4)); break; }
    case 0x2b: { MEMOP(8) ADDR(8) PUSH(ld(vm->mem + ea_, 8)); break; }
    case 0x2c: { MEMOP(1) ADDR(1) PUSH((uint32_t)(int32_t)(int8_t)vm->mem[ea_]); break; }
    case 0x2d: { MEMOP(1) ADDR(1) PUSH((uint32_t)vm->mem[ea_]); break; }
    case 0x2e: { MEMOP(2) ADDR(2) PUSH((uint32_t)(int32_t)(int16_t)ld(vm->mem + ea_, 2)); break; }
    case 0x2f: { MEMOP(2) ADDR(2) PUSH((uint32_t)ld(vm->mem + ea_, 2)); break; }
    case 0x30: { MEMOP(1) ADDR(1) PUSH((uint64_t)(int64_t)(int8_t)vm->mem[ea_]); break; }
    case 0x31: { MEMOP(1) ADDR(1) PUSH((uint64_t)vm->mem[ea_]); break; }
    case 0x32: { MEMOP(2) ADDR(2) PUSH((uint64_t)(int64_t)(int16_t)ld(vm->mem + ea_, 2)); break; }
    case 0x33: { MEMOP(2) ADDR(2) PUSH(ld(vm->mem + ea_, 2)); break; }
    case 0x34: { MEMOP(4) ADDR(4) PUSH((uint64_t)(int64_t)(int32_t)ld(vm->mem + ea_, 4)); break; }
    case 0x35: { MEMOP(4) ADDR(4) PUSH(ld(vm->mem + ea_, 4)); break; }
    /* stores */
    case 0x36: case 0x38: { uint64_t v = POP(); MEMOP(4) ADDR(4) st(vm->mem + ea_, v, 4); break; }
    case 0x37: case 0x39: { uint64_t v = POP(); MEMOP(8) ADDR(8) st(vm->mem + ea_, v, 8); break; }
    case 0x3a: case 0x3c: { uint64_t v = POP(); MEMOP(1) ADDR(1) st(vm->mem + ea_, v, 1); break; }
    case 0x3b: case 0x3d: { uint64_t v = POP(); MEMOP(2) ADDR(2) st(vm->mem + ea_, v, 2); break; }
    case 0x3e: { uint64_t v = POP(); MEMOP(4) ADDR(4) st(vm->mem + ea_, v, 4); break; }
    case 0x3f: pc++; PUSH((uint32_t)vm->mem_pages); break;
    case 0x40: { /* memory.grow */
      pc++;
      uint32_t d = (uint32_t)POP();
      uint64_t old = vm->mem_pages;
      if (!vm->has_mem || old + d > vm->mem_max) {
        PUSH(UINT32_MAX);
        break;
      }
      if (d) {
        vm->mem = xrealloc(vm->mem, (size_t)((old + d) * PAGE));
        memset(vm->mem + old * PAGE, 0, (size_t)d * PAGE);
        vm->mem_pages = old + d;
      }
      PUSH((uint32_t)old);
      break;
    }
    case 0x41: PUSH((uint32_t)(int32_t)fs64(&pc)); break;
    case 0x42: PUSH((uint64_t)fs64(&pc)); break;
    case 0x43: PUSH(ld(pc, 4)); pc += 4; break;
    case 0x44: PUSH(ld(pc, 8)); pc += 8; break;
    /* i32 comparisons */
    case 0x45: TOP() = (uint32_t)TOP() == 0; break;
    case 0x46: CMP32(a == b)
    case 0x47: CMP32(a != b)
    case 0x48: CMP32((int32_t)a < (int32_t)b)
    case 0x49: CMP32(a < b)
    case 0x4a: CMP32((int32_t)a > (int32_t)b)
    case 0x4b: CMP32(a > b)
    case 0x4c: CMP32((int32_t)a <= (int32_t)b)
    case 0x4d: CMP32(a <= b)
    case 0x4e: CMP32((int32_t)a >= (int32_t)b)
    case 0x4f: CMP32(a >= b)
    /* i64 comparisons */
    case 0x50: TOP() = TOP() == 0; break;
    case 0x51: CMP64(a == b)
    case 0x52: CMP64(a != b)
    case 0x53: CMP64((int64_t)a < (int64_t)b)
    case 0x54: CMP64(a < b)
    case 0x55: CMP64((int64_t)a > (int64_t)b)
    case 0x56: CMP64(a > b)
    case 0x57: CMP64((int64_t)a <= (int64_t)b)
    case 0x58: CMP64(a <= b)
    case 0x59: CMP64((int64_t)a >= (int64_t)b)
    case 0x5a: CMP64(a >= b)
    /* f32/f64 comparisons */
    case 0x5b: FCMP32(a == b)
    case 0x5c: FCMP32(a != b)
    case 0x5d: FCMP32(a < b)
    case 0x5e: FCMP32(a > b)
    case 0x5f: FCMP32(a <= b)
    case 0x60: FCMP32(a >= b)
    case 0x61: FCMP64(a == b)
    case 0x62: FCMP64(a != b)
    case 0x63: FCMP64(a < b)
    case 0x64: FCMP64(a > b)
    case 0x65: FCMP64(a <= b)
    case 0x66: FCMP64(a >= b)
    /* i32 arithmetic */
    case 0x67: TOP() = clz32((uint32_t)TOP()); break;
    case 0x68: TOP() = ctz32((uint32_t)TOP()); break;
    case 0x69: TOP() = pop32((uint32_t)TOP()); break;
    case 0x6a: BIN32(a + b)
    case 0x6b: BIN32(a - b)
    case 0x6c: BIN32(a * b)
    case 0x6d: {
      int32_t b = (int32_t)POP(), a = (int32_t)POP();
      if (b == 0) TRAP("integer divide by zero");
      if (a == INT32_MIN && b == -1) TRAP("integer overflow");
      PUSH((uint32_t)(a / b));
      break;
    }
    case 0x6e: {
      uint32_t b = (uint32_t)POP(), a = (uint32_t)POP();
      if (b == 0) TRAP("integer divide by zero");
      PUSH(a / b);
      break;
    }
    case 0x6f: {
      int32_t b = (int32_t)POP(), a = (int32_t)POP();
      if (b == 0) TRAP("integer divide by zero");
      PUSH((uint32_t)(b == -1 ? 0 : a % b));
      break;
    }
    case 0x70: {
      uint32_t b = (uint32_t)POP(), a = (uint32_t)POP();
      if (b == 0) TRAP("integer divide by zero");
      PUSH(a % b);
      break;
    }
    case 0x71: BIN32(a & b)
    case 0x72: BIN32(a | b)
    case 0x73: BIN32(a ^ b)
    case 0x74: BIN32(a << (b & 31))
    case 0x75: BIN32((uint32_t)((int32_t)a >> (b & 31)))
    case 0x76: BIN32(a >> (b & 31))
    case 0x77: BIN32(rotl32(a, b))
    case 0x78: BIN32(rotr32(a, b))
    /* i64 arithmetic */
    case 0x79: TOP() = clz64(TOP()); break;
    case 0x7a: TOP() = ctz64(TOP()); break;
    case 0x7b: TOP() = pop64(TOP()); break;
    case 0x7c: BIN64(a + b)
    case 0x7d: BIN64(a - b)
    case 0x7e: BIN64(a * b)
    case 0x7f: {
      int64_t b = (int64_t)POP(), a = (int64_t)POP();
      if (b == 0) TRAP("integer divide by zero");
      if (a == INT64_MIN && b == -1) TRAP("integer overflow");
      PUSH((uint64_t)(a / b));
      break;
    }
    case 0x80: {
      uint64_t b = POP(), a = POP();
      if (b == 0) TRAP("integer divide by zero");
      PUSH(a / b);
      break;
    }
    case 0x81: {
      int64_t b = (int64_t)POP(), a = (int64_t)POP();
      if (b == 0) TRAP("integer divide by zero");
      PUSH((uint64_t)(b == -1 ? 0 : a % b));
      break;
    }
    case 0x82: {
      uint64_t b = POP(), a = POP();
      if (b == 0) TRAP("integer divide by zero");
      PUSH(a % b);
      break;
    }
    case 0x83: BIN64(a & b)
    case 0x84: BIN64(a | b)
    case 0x85: BIN64(a ^ b)
    case 0x86: BIN64(a << (b & 63))
    case 0x87: BIN64((uint64_t)((int64_t)a >> (b & 63)))
    case 0x88: BIN64(a >> (b & 63))
    case 0x89: BIN64(rotl64(a, b))
    case 0x8a: BIN64(rotr64(a, b))
    /* f32 arithmetic */
    case 0x8b: TOP() = TOP() & 0x7fffffffu; break;
    case 0x8c: TOP() = (TOP() ^ 0x80000000u) & 0xffffffffu; break;
    case 0x8d: FUN32(ceilf(a))
    case 0x8e: FUN32(floorf(a))
    case 0x8f: FUN32(truncf(a))
    case 0x90: FUN32(wnearestf(a))
    case 0x91: FUN32(sqrtf(a))
    case 0x92: FBIN32(a + b)
    case 0x93: FBIN32(a - b)
    case 0x94: FBIN32(a * b)
    case 0x95: FBIN32(a / b)
    case 0x96: FBIN32(wminf(a, b))
    case 0x97: FBIN32(wmaxf(a, b))
    case 0x98: {
      uint64_t b = POP(), a = POP();
      PUSH((a & 0x7fffffffu) | (b & 0x80000000u));
      break;
    }
    /* f64 arithmetic */
    case 0x99: TOP() &= 0x7fffffffffffffffull; break;
    case 0x9a: TOP() ^= 0x8000000000000000ull; break;
    case 0x9b: FUN64(ceil(a))
    case 0x9c: FUN64(floor(a))
    case 0x9d: FUN64(trunc(a))
    case 0x9e: FUN64(wnearest(a))
    case 0x9f: FUN64(sqrt(a))
    case 0xa0: FBIN64(a + b)
    case 0xa1: FBIN64(a - b)
    case 0xa2: FBIN64(a * b)
    case 0xa3: FBIN64(a / b)
    case 0xa4: FBIN64(wmin(a, b))
    case 0xa5: FBIN64(wmax(a, b))
    case 0xa6: {
      uint64_t b = POP(), a = POP();
      PUSH((a & 0x7fffffffffffffffull) | (b & 0x8000000000000000ull));
      break;
    }
    /* conversions */
    case 0xa7: TOP() = (uint32_t)TOP(); break;
    case 0xa8: TOP() = (uint32_t)(int32_t)trunc_s(f32_of(TOP()), 32, 0); break;
    case 0xa9: TOP() = (uint32_t)trunc_u(f32_of(TOP()), 32, 0); break;
    case 0xaa: TOP() = (uint32_t)(int32_t)trunc_s(f64_of(TOP()), 32, 0); break;
    case 0xab: TOP() = (uint32_t)trunc_u(f64_of(TOP()), 32, 0); break;
    case 0xac: TOP() = (uint64_t)(int64_t)(int32_t)(uint32_t)TOP(); break;
    case 0xad: TOP() = (uint32_t)TOP(); break;
    case 0xae: TOP() = (uint64_t)trunc_s(f32_of(TOP()), 64, 0); break;
    case 0xaf: TOP() = trunc_u(f32_of(TOP()), 64, 0); break;
    case 0xb0: TOP() = (uint64_t)trunc_s(f64_of(TOP()), 64, 0); break;
    case 0xb1: TOP() = trunc_u(f64_of(TOP()), 64, 0); break;
    case 0xb2: TOP() = of_f32((float)(int32_t)(uint32_t)TOP()); break;
    case 0xb3: TOP() = of_f32((float)(uint32_t)TOP()); break;
    case 0xb4: TOP() = of_f32((float)(int64_t)TOP()); break;
    case 0xb5: TOP() = of_f32((float)TOP()); break;
    case 0xb6: TOP() = of_f32((float)f64_of(TOP())); break;
    case 0xb7: TOP() = of_f64((double)(int32_t)(uint32_t)TOP()); break;
    case 0xb8: TOP() = of_f64((double)(uint32_t)TOP()); break;
    case 0xb9: TOP() = of_f64((double)(int64_t)TOP()); break;
    case 0xba: TOP() = of_f64((double)TOP()); break;
    case 0xbb: TOP() = of_f64((double)f32_of(TOP())); break;
    case 0xbc: TOP() = (uint32_t)TOP(); break; /* i32.reinterpret_f32 */
    case 0xbd: break;                          /* i64.reinterpret_f64 */
    case 0xbe: TOP() = (uint32_t)TOP(); break; /* f32.reinterpret_i32 */
    case 0xbf: break;                          /* f64.reinterpret_i64 */
    /* sign extension */
    case 0xc0: TOP() = (uint32_t)(int32_t)(int8_t)TOP(); break;
    case 0xc1: TOP() = (uint32_t)(int32_t)(int16_t)TOP(); break;
    case 0xc2: TOP() = (uint64_t)(int64_t)(int8_t)TOP(); break;
    case 0xc3: TOP() = (uint64_t)(int64_t)(int16_t)TOP(); break;
    case 0xc4: TOP() = (uint64_t)(int64_t)(int32_t)TOP(); break;
    /* references */
    case 0xd0: pc++; PUSH(NULL_FUNC); break;
    case 0xd1: TOP() = (uint32_t)TOP() == NULL_FUNC; break;
    case 0xd2: PUSH(fu32(&pc)); break;
    case 0xfc: {
      uint32_t sub = fu32(&pc);
      switch (sub) {
      case 0: TOP() = (uint32_t)(int32_t)trunc_s(f32_of(TOP()), 32, 1); break;
      case 1: TOP() = (uint32_t)trunc_u(f32_of(TOP()), 32, 1); break;
      case 2: TOP() = (uint32_t)(int32_t)trunc_s(f64_of(TOP()), 32, 1); break;
      case 3: TOP() = (uint32_t)trunc_u(f64_of(TOP()), 32, 1); break;
      case 4: TOP() = (uint64_t)trunc_s(f32_of(TOP()), 64, 1); break;
      case 5: TOP() = trunc_u(f32_of(TOP()), 64, 1); break;
      case 6: TOP() = (uint64_t)trunc_s(f64_of(TOP()), 64, 1); break;
      case 7: TOP() = trunc_u(f64_of(TOP()), 64, 1); break;
      case 8: { /* memory.init */
        uint32_t seg = fu32(&pc);
        pc++;
        uint32_t n = (uint32_t)POP(), s = (uint32_t)POP(), d = (uint32_t)POP();
        if (seg >= vm->ndatas) TRAP("data segment out of range");
        Segment *g = &vm->datas[seg];
        uint32_t len = g->dropped ? 0 : g->len;
        if ((uint64_t)s + n > len || (uint64_t)d + n > vm->mem_pages * PAGE) TRAP("out of bounds memory access");
        memcpy(vm->mem + d, g->data + s, n);
        break;
      }
      case 9: {
        uint32_t seg = fu32(&pc);
        if (seg < vm->ndatas) vm->datas[seg].dropped = 1;
        break;
      }
      case 10: { /* memory.copy */
        pc += 2;
        uint32_t n = (uint32_t)POP(), s = (uint32_t)POP(), d = (uint32_t)POP();
        uint64_t lim = vm->mem_pages * PAGE;
        if ((uint64_t)s + n > lim || (uint64_t)d + n > lim) TRAP("out of bounds memory access");
        memmove(vm->mem + d, vm->mem + s, n);
        break;
      }
      case 11: { /* memory.fill */
        pc++;
        uint32_t n = (uint32_t)POP(), v = (uint32_t)POP(), d = (uint32_t)POP();
        if ((uint64_t)d + n > vm->mem_pages * PAGE) TRAP("out of bounds memory access");
        memset(vm->mem + d, (int)(v & 0xff), n);
        break;
      }
      case 12: { /* table.init */
        uint32_t seg = fu32(&pc);
        (void)fu32(&pc);
        uint32_t n = (uint32_t)POP(), s = (uint32_t)POP(), d = (uint32_t)POP();
        if (seg >= vm->nelems) TRAP("element segment out of range");
        Segment *g = &vm->elems[seg];
        uint32_t len = g->dropped ? 0 : g->len;
        if ((uint64_t)s + n > len || (uint64_t)d + n > vm->table_len) TRAP("out of bounds table access");
        memcpy(vm->table + d, (const uint32_t *)g->data + s, sizeof(uint32_t) * n);
        break;
      }
      case 13: {
        uint32_t seg = fu32(&pc);
        if (seg < vm->nelems) vm->elems[seg].dropped = 1;
        break;
      }
      case 14: { /* table.copy */
        (void)fu32(&pc);
        (void)fu32(&pc);
        uint32_t n = (uint32_t)POP(), s = (uint32_t)POP(), d = (uint32_t)POP();
        if ((uint64_t)s + n > vm->table_len || (uint64_t)d + n > vm->table_len) TRAP("out of bounds table access");
        memmove(vm->table + d, vm->table + s, sizeof(uint32_t) * n);
        break;
      }
      case 15: { /* table.grow */
        (void)fu32(&pc);
        uint32_t n = (uint32_t)POP();
        uint32_t init = (uint32_t)POP();
        uint32_t old = vm->table_len;
        if ((uint64_t)old + n > vm->table_max) {
          PUSH(UINT32_MAX);
          break;
        }
        vm->table = xrealloc(vm->table, sizeof(uint32_t) * ((size_t)old + n + 1));
        for (uint32_t k = 0; k < n; k++) vm->table[old + k] = init;
        vm->table_len = old + n;
        PUSH(old);
        break;
      }
      case 16: (void)fu32(&pc); PUSH(vm->table_len); break;
      case 17: { /* table.fill */
        (void)fu32(&pc);
        uint32_t n = (uint32_t)POP(), v = (uint32_t)POP(), d = (uint32_t)POP();
        if ((uint64_t)d + n > vm->table_len) TRAP("out of bounds table access");
        for (uint32_t k = 0; k < n; k++) vm->table[d + k] = v;
        break;
      }
      default: TRAP("unsupported 0xfc instruction %u", sub);
      }
      break;
    }
    default: TRAP("unsupported opcode 0x%02x", op);
    }
  }
}

/* ------------------------------------------------------------------ */
/* Driver                                                              */

static uint8_t *read_file(const char *path, size_t *len) {
  FILE *f = fopen(path, "rb");
  if (!f) die(2, "cannot open %s", path);
  size_t cap = 1 << 16, n = 0;
  uint8_t *buf = xmalloc(cap);
  for (;;) {
    if (n == cap) buf = xrealloc(buf, cap *= 2);
    size_t got = fread(buf + n, 1, cap - n, f);
    if (!got) break;
    n += got;
  }
  if (ferror(f)) die(2, "cannot read %s", path);
  fclose(f);
  *len = n;
  return buf;
}

static int find_export(Vm *vm, const char *name) {
  for (uint32_t i = 0; i < vm->nexports; i++)
    if (vm->exports[i].kind == 0 && !strcmp(vm->exports[i].name, name)) return (int)vm->exports[i].index;
  return -1;
}

static void print_value(uint8_t t, uint64_t v) {
  switch (t) {
  case T_I32: printf("%" PRId32 "\n", (int32_t)(uint32_t)v); break;
  case T_I64: printf("%" PRId64 "\n", (int64_t)v); break;
  case T_F32: printf("%.9g\n", (double)f32_of(v)); break;
  case T_F64: printf("%.17g\n", f64_of(v)); break;
  default: printf("ref %" PRIu64 "\n", v); break;
  }
}

int main(int argc, char **argv) {
  const char *invoke_name = NULL, *path = NULL;
  int print = 0, dump = 0;
  uint64_t fuel = 0;
  size_t stack = (size_t)16 << 20;
  int i = 1;
  for (; i < argc; i++) {
    if (!strcmp(argv[i], "--invoke") && i + 1 < argc) invoke_name = argv[++i];
    else if (!strcmp(argv[i], "--print")) print = 1;
    else if (!strcmp(argv[i], "--dump")) dump = 1;
    else if (!strcmp(argv[i], "--report") && i + 1 < argc) report_path = argv[++i];
    else if (!strcmp(argv[i], "--fuel") && i + 1 < argc) fuel = strtoull(argv[++i], NULL, 10);
    else if (!strcmp(argv[i], "--stack") && i + 1 < argc) stack = (size_t)strtoull(argv[++i], NULL, 10);
    else if (argv[i][0] == '-' && argv[i][1] == '-') die(2, "unknown option %s", argv[i]);
    else {
      path = argv[i++];
      break;
    }
  }
  if (!path) die(2, "usage: vwasm [--invoke NAME] [--print] [--fuel N] [--stack N] [--dump] FILE.wasm [ARG...]");
  size_t len, mlen;
  uint8_t *file = read_file(path, &len);
  const uint8_t *mod = find_core_module(file, len, &mlen);
  Vm vm;
  memset(&vm, 0, sizeof vm);
  load(&vm, mod, mlen);
  if (dump) {
    for (uint32_t k = 0; k < vm.nimports; k++)
      printf("import %s %s%s\n", vm.funcs[k].imp_module, vm.funcs[k].imp_name,
             vm.funcs[k].host == host_missing ? " (missing)" : "");
    for (uint32_t k = 0; k < vm.nexports; k++) printf("export %u %s\n", vm.exports[k].kind, vm.exports[k].name);
    return 0;
  }
  vm.stack_cap = stack;
  vm.stack = xmalloc(sizeof(uint64_t) * stack);
  vm.fuel = fuel;
  vm.unlimited = fuel == 0;
  if (vm.start >= 0) {
    uint64_t none[1];
    invoke(&vm, (uint32_t)vm.start, none, none);
  }
  static const char *defaults[] = {"wasi:cli/run@0.2.0#run", "vesper:program/entry@0.1.0#main", "_start", "main"};
  int idx = -1;
  if (invoke_name) {
    idx = find_export(&vm, invoke_name);
    if (idx < 0) die(2, "no exported function %s", invoke_name);
  } else {
    for (size_t k = 0; k < 4 && idx < 0; k++)
      if ((idx = find_export(&vm, defaults[k])) >= 0) invoke_name = defaults[k];
    if (idx < 0) die(2, "no entry export; pass --invoke NAME");
  }
  const FuncType *ft = &vm.types[vm.funcs[idx].type];
  if ((uint32_t)(argc - i) != ft->np) die(2, "%s takes %u arguments, got %d", invoke_name, ft->np, argc - i);
  uint64_t *args = xmalloc(sizeof(uint64_t) * (ft->np + 1));
  for (uint32_t k = 0; k < ft->np; k++) {
    const char *a = argv[i + k];
    switch (ft->p[k]) {
    case T_I32: args[k] = (uint32_t)strtoll(a, NULL, 0); break;
    case T_I64: args[k] = (uint64_t)strtoll(a, NULL, 0); break;
    case T_F32: args[k] = of_f32(strtof(a, NULL)); break;
    case T_F64: args[k] = of_f64(strtod(a, NULL)); break;
    default: die(2, "cannot pass a reference argument");
    }
  }
  uint64_t *results = xmalloc(sizeof(uint64_t) * (ft->nr + 1));
  invoke(&vm, (uint32_t)idx, args, results);
  fflush(stdout);
  if (print)
    for (uint32_t k = 0; k < ft->nr; k++) print_value(ft->r[k], results[k]);
  fflush(stdout);
  if (report_path) {
    FILE *r = fopen(report_path, "wb");
    if (!r) die(2, "cannot write %s", report_path);
    fputs("ok\n", r);
    for (uint32_t k = 0; k < ft->nr; k++) {
      uint64_t v = results[k];
      if (ft->r[k] == T_I32 || ft->r[k] == T_F32) v &= 0xffffffffu;
      fprintf(r, "%016" PRIx64 "\n", v);
    }
    uint64_t h = 0xcbf29ce484222325ull;
    size_t bytes = (size_t)(vm.mem_pages * PAGE);
    for (size_t k = 0; k < bytes; k++) h = (h ^ vm.mem[k]) * 0x100000001b3ull;
    fprintf(r, "memory %" PRIu64 " %016" PRIx64 "\n", vm.mem_pages, h);
    fclose(r);
  }
  if (!strcmp(invoke_name, defaults[0]) && ft->nr == 1) return (uint32_t)results[0] == 0 ? 0 : 1;
  return 0;
}
