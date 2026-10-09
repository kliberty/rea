/*
 * Tiny alias stub for the shared-library variant.
 *
 * All it holds is an empty template slot; `aliasmux patch` copies this
 * binary once per alias and writes the alias into the slot. Everything else
 * (parsing, expansion, pipes, signals) happens in libaliasmux_shared.so.
 *
 * Build (scripts/build_variants.py does this):
 *   cc -O2 -o aliasmux-cstub stub.c -L<dir> -laliasmux_shared -Wl,-rpath,'$ORIGIN/lib'
 */
#include "aliasmux.h"

/*
 * The slot. `used` keeps it even though nothing in this file reads it, and
 * the marker initializer is what `aliasmux patch` searches for. Taking its
 * address and passing it to the library is enough to stop the compiler from
 * assuming its contents: the library is opaque to it, so the bytes are read
 * at run time, whatever the file holds.
 *
 * The initializer spells out the 16 marker bytes via a string literal; C
 * allows a 16-char literal to fill a 16-byte array exactly, without a NUL.
 */
__attribute__((used))
static const struct aliasmux_slot slot = {
    .marker = ALIASMUX_SLOT_MARKER,
};

/*
 * Hand the slot and argv to the library.
 *
 * :param argc: argument count
 * :param argv: arguments; argv[0] is ignored (the slot holds the name)
 * :return: never; the library execs or exits
 */
int main(int argc, char **argv) {
    aliasmux_run_slot(&slot, argc, argv);
}
