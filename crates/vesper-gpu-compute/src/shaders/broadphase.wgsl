// Sort-and-sweep broadphase over axis-aligned boxes (Kinetic, design §36.3).
//
// `boxes` holds six f32 bit patterns per box: min x, y, z then max x, y, z.
// Every comparison is done on integers: `ord` maps the bits of a finite f32
// to a u32 whose unsigned order is the float order, with -0 mapped like +0
// (IEEE -0 == +0). The host rejects NaN and infinities first, so the pair set
// is exact: no rounding, no flush-to-zero, no fused operation can change it.
//
// Entry points, run in this order by the host:
//   keys  : key[i] = ord(min x of box i), id[i] = i; padding sorts last.
//   sort  : one bitonic merge step (j, k) over (key, id) pairs, ascending.
//   sweep : for sorted position i, every later j with key[j] <= ord(max x of
//           box id[i]) whose y and z intervals also overlap; mode 0 counts
//           them, mode 1 writes them at offsets[i].
//   scan  : exclusive prefix sum of the counts in one workgroup; offsets[n]
//           is the total.
struct Params {
    n: u32,
    a: u32,
    b: u32,
    c: u32,
}
var<immediate> p: Params;

fn ord(bits: u32) -> u32 {
    var v = bits;
    if v == 0x80000000u {
        v = 0u;
    }
    if (v & 0x80000000u) != 0u {
        return ~v;
    }
    return v | 0x80000000u;
}
