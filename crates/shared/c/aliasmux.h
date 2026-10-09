/*
 * C interface to libaliasmux_shared.so (crates/shared/src/lib.rs).
 *
 * Constraint on change: the slot layout here must match
 * crates/core/src/slot.rs byte for byte. The marker string is the version
 * check; if the layout ever changes, both sides bump "v1" together. The
 * static assertion below catches size drift at compile time, and the
 * library rejects a mismatched marker at run time.
 */
#ifndef ALIASMUX_H
#define ALIASMUX_H

#include <stddef.h>

/* Payload capacity in bytes; must equal slot::CAPACITY. */
#define ALIASMUX_SLOT_CAPACITY 4096

/* The 16 marker bytes (no terminating NUL is stored); must equal slot::MARKER. */
#define ALIASMUX_SLOT_MARKER "aliasmux:slot:v1"

/* The template slot. Only byte arrays, so there is no padding on any ABI. */
struct aliasmux_slot {
    unsigned char marker[16];                       /* ALIASMUX_SLOT_MARKER */
    unsigned char len[4];                           /* payload length, u32 little-endian; 0 = unpatched */
    unsigned char data[ALIASMUX_SLOT_CAPACITY];     /* "name\ntemplate", zero padded */
};

/* Size drift would make the library read past the stub's slot. */
_Static_assert(sizeof(struct aliasmux_slot) == 16 + 4 + ALIASMUX_SLOT_CAPACITY, "slot layout must have no padding");

/*
 * Run the alias stored in a slot with the caller's arguments.
 *
 * Never returns: becomes the target program, or exits with its outcome.
 *
 * :param slot: the caller's slot (aliasmux patch fills it in the file)
 * :param argc: argc as received by main
 * :param argv: argv as received by main
 */
_Noreturn void aliasmux_run_slot(const struct aliasmux_slot *slot, int argc, char **argv);

#endif
