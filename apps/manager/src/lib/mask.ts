// SPDX-License-Identifier: GPL-3.0-only
// Seat-mask helpers, mirror of `seat_core::ids::SeatMask`. JS bit operators are signed
// 32-bit, so every result is brought back to an unsigned u32 with `>>> 0`.

import { MAX_SEATS, SHARED_MASK, SPARE_MASK, type SeatMask, type SeatNumber } from './types.ts';

export const ALL_SEATS_BITS = allUpTo(MAX_SEATS);

export function u32(m: number): SeatMask {
  return m >>> 0;
}

export function bit(seat: SeatNumber): SeatMask {
  return u32(1 << (seat - 1));
}

export function isShared(m: SeatMask): boolean {
  return u32(m) === SHARED_MASK;
}

export function isSpare(m: SeatMask): boolean {
  return u32(m) === SPARE_MASK;
}

export function contains(m: SeatMask, seat: SeatNumber): boolean {
  return (u32(m) & bit(seat)) !== 0;
}

export function allUpTo(count: number): SeatMask {
  let m = 0;
  for (let s = 1; s <= Math.min(count, MAX_SEATS); s++) m |= 1 << (s - 1);
  return u32(m);
}

export function fromSeats(seats: Iterable<SeatNumber>): SeatMask {
  let m = 0;
  for (const s of seats) m |= bit(s);
  return u32(m);
}

export function insert(m: SeatMask, seat: SeatNumber): SeatMask {
  return u32(u32(m) | bit(seat));
}

/** Removing a seat from SHARED first expands SHARED to every seat (like the Rust side). */
export function remove(m: SeatMask, seat: SeatNumber): SeatMask {
  const base = isShared(m) ? ALL_SEATS_BITS : u32(m);
  return u32(base & ~bit(seat));
}

/** Seats in the mask (shared = every seat up to MAX_SEATS). */
export function seatsOf(m: SeatMask): SeatNumber[] {
  const out: SeatNumber[] = [];
  for (let s = 1; s <= MAX_SEATS; s++) if (contains(m, s)) out.push(s);
  return out;
}

export function count(m: SeatMask): number {
  return seatsOf(m).length;
}

/** The only seat, when exactly one seat bit is set and the mask is not shared. */
export function single(m: SeatMask): SeatNumber | null {
  if (isShared(m)) return null;
  const s = seatsOf(m);
  return s.length === 1 ? s[0] : null;
}

/** Bits above `seatCount` (other than SHARED). */
export function beyond(m: SeatMask, seatCount: number): boolean {
  return !isShared(m) && (u32(m) & ~allUpTo(seatCount)) !== 0;
}

/** Shared masks covering every configured seat count as "all seats" in the UI. */
export function coversAll(m: SeatMask, seatNumbers: SeatNumber[]): boolean {
  return isShared(m) || (seatNumbers.length > 0 && seatNumbers.every((s) => contains(m, s)));
}
