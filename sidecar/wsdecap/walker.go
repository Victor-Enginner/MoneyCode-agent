// SPDX-License-Identifier: Apache-2.0
// Walker protobuf genérico sem necessidade de arquivo .proto prévio.
// Percorre wire types varint (0) e length-delimited (2) até profundidade 8.
// Coleta candidatos cujo valor inteiro esteja no intervalo válido da roleta (0..=36).

package wsdecap

import "strconv"

// Cand representa um candidato (caminho de campo protobuf, valor 0..=36).
type Cand struct {
	Path string
	Val  uint64
}

// Walk percorre um buffer protobuf binário e retorna todos os candidatos com valor em 0..=36.
func Walk(b []byte) []Cand {
	var out []Cand
	walk(b, "", &out, 0)
	return out
}

func walk(b []byte, path string, out *[]Cand, depth int) {
	if depth > 8 {
		return
	}
	for len(b) > 0 {
		key, n := uvarint(b)
		if n <= 0 {
			return
		}
		b = b[n:]
		field, wire := key>>3, key&7
		if field == 0 {
			return
		}
		p := path + "/" + strconv.FormatUint(field, 10)
		switch wire {
		case 0: // varint
			v, m := uvarint(b)
			if m <= 0 {
				return
			}
			b = b[m:]
			if v <= 36 {
				*out = append(*out, Cand{Path: p, Val: v})
			}
		case 2: // length-delimited
			ln, m := uvarint(b)
			if m <= 0 {
				return
			}
			b = b[m:]
			if int(ln) > len(b) {
				return
			}
			walk(b[:ln], p, out, depth+1)
			b = b[ln:]
		case 1: // 64-bit
			if len(b) < 8 {
				return
			}
			b = b[8:]
		case 5: // 32-bit
			if len(b) < 4 {
				return
			}
			b = b[4:]
		default:
			return
		}
	}
}

func uvarint(buf []byte) (uint64, int) {
	var x uint64
	var s uint
	for i, b := range buf {
		if i == 10 {
			return 0, 0
		}
		if b < 0x80 {
			if i == 9 && b > 1 {
				return 0, 0
			}
			return x | uint64(b)<<s, i + 1
		}
		x |= uint64(b&0x7f) << s
		s += 7
	}
	return 0, 0
}
