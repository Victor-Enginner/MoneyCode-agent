// SPDX-License-Identifier: Apache-2.0
// Walker protobuf genérico e dumper de árvore sem necessidade de arquivo .proto prévio.
// Ignora frames de controle (<= 8 bytes), detecta/infla zlib (78 9c / 78 01) e percorre até profundidade 8.
// Inclui FormatTree e HexHead para inspeção de frames de resultado (--dump-frames) e aposta (--dump-op).

package wsdecap

import (
	"bytes"
	"compress/zlib"
	"encoding/hex"
	"fmt"
	"io"
	"strconv"
	"strings"
	"unicode/utf8"
)

// PlaytechNumberOffset é o offset confirmado em gameRoundOver (#3.16.3.1.2) e GameEventRequest (#3.118.1).
const PlaytechNumberOffset uint64 = 100

// MinFramePayloadLen descarta frames de controle WebSocket (ping/pong/close de 1..8 bytes) no fluxo real.
const MinFramePayloadLen = 8

// Cand representa um candidato (caminho de campo protobuf, valor 0..=36).
type Cand struct {
	Path string
	Val  uint64
}

// MaybeInflate detecta cabeçalho zlib (0x78 0x9c, 0x78 0x01, 0x78 0xda) e descomprime se aplicável.
func MaybeInflate(b []byte) ([]byte, bool) {
	if len(b) >= 2 && b[0] == 0x78 && (b[1] == 0x9c || b[1] == 0x01 || b[1] == 0xda || b[1] == 0x5e) {
		r, err := zlib.NewReader(bytes.NewReader(b))
		if err == nil {
			defer r.Close()
			inflated, readErr := io.ReadAll(io.LimitReader(r, 256*1024))
			if readErr == nil && len(inflated) > 0 {
				return inflated, true
			}
		}
	}
	return b, false
}

// HexHead retorna os primeiros maxBytes do buffer em formato hexadecimal espaçado.
func HexHead(b []byte, maxBytes int) string {
	if len(b) == 0 {
		return ""
	}
	if maxBytes > 0 && len(b) > maxBytes {
		b = b[:maxBytes]
	}
	rawHex := hex.EncodeToString(b)
	var sb strings.Builder
	for i := 0; i < len(rawHex); i += 2 {
		if i > 0 {
			sb.WriteByte(' ')
		}
		sb.WriteString(rawHex[i : i+2])
	}
	return sb.String()
}

// Walk percorre um buffer protobuf binário e retorna todos os candidatos com valor em 0..=36.
func Walk(b []byte) []Cand {
	payload, _ := MaybeInflate(b)
	var out []Cand
	walk(payload, "", &out, 0)
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
			} else if v >= PlaytechNumberOffset && v <= PlaytechNumberOffset+36 {
				*out = append(*out, Cand{Path: p + "[offset100]", Val: v - PlaytechNumberOffset})
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

// FormatTree decodifica o buffer protobuf em árvore legível até profundidade 8 (M2 / --dump-op / --dump-frames).
func FormatTree(b []byte) string {
	payload, inflated := MaybeInflate(b)
	var sb strings.Builder
	if inflated {
		sb.WriteString("[zlib inflated]\n")
	}
	dumpTree(&sb, payload, "", 0)
	return sb.String()
}

func dumpTree(sb *strings.Builder, b []byte, path string, depth int) {
	if depth > 8 {
		return
	}
	indent := strings.Repeat("  ", depth)
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
		case 0:
			v, m := uvarint(b)
			if m <= 0 {
				return
			}
			b = b[m:]
			fmt.Fprintf(sb, "%s%d (%s): varint %d\n", indent, field, p, v)
		case 2:
			ln, m := uvarint(b)
			if m <= 0 {
				return
			}
			b = b[m:]
			if int(ln) > len(b) {
				return
			}
			sub := b[:ln]
			b = b[ln:]
			if isLikelySubMessage(sub) {
				fmt.Fprintf(sb, "%s%d (%s): msg (%d bytes)\n", indent, field, p, len(sub))
				dumpTree(sb, sub, p, depth+1)
			} else if isPrintableUTF8(sub) {
				fmt.Fprintf(sb, "%s%d (%s): str %q\n", indent, field, p, string(sub))
			} else {
				fmt.Fprintf(sb, "%s%d (%s): bytes [%s]\n", indent, field, p, HexHead(sub, 24))
			}
		case 1:
			if len(b) < 8 {
				return
			}
			fmt.Fprintf(sb, "%s%d (%s): fixed64 [%s]\n", indent, field, p, HexHead(b[:8], 8))
			b = b[8:]
		case 5:
			if len(b) < 4 {
				return
			}
			fmt.Fprintf(sb, "%s%d (%s): fixed32 [%s]\n", indent, field, p, HexHead(b[:4], 4))
			b = b[4:]
		default:
			return
		}
	}
}

func isLikelySubMessage(b []byte) bool {
	if len(b) < 2 {
		return false
	}
	rem := b
	fields := 0
	for len(rem) > 0 {
		key, n := uvarint(rem)
		if n <= 0 {
			return false
		}
		rem = rem[n:]
		field, wire := key>>3, key&7
		if field == 0 || field > 2048 {
			return false
		}
		switch wire {
		case 0:
			_, m := uvarint(rem)
			if m <= 0 {
				return false
			}
			rem = rem[m:]
		case 2:
			ln, m := uvarint(rem)
			if m <= 0 {
				return false
			}
			rem = rem[m:]
			if int(ln) > len(rem) {
				return false
			}
			rem = rem[ln:]
		case 1:
			if len(rem) < 8 {
				return false
			}
			rem = rem[8:]
		case 5:
			if len(rem) < 4 {
				return false
			}
			rem = rem[4:]
		default:
			return false
		}
		fields++
	}
	return fields > 0
}

func isPrintableUTF8(b []byte) bool {
	if len(b) == 0 || !utf8.Valid(b) {
		return false
	}
	for _, r := range string(b) {
		if r < 0x20 && r != '\n' && r != '\r' && r != '\t' {
			return false
		}
	}
	return true
}

// ExtractAtPath extrai o valor varint no caminho exato (ex.: "/3/103/1/1") se presente no frame.
func ExtractAtPath(b []byte, targetPath string) (uint64, bool) {
	cleanTarget := strings.TrimSuffix(targetPath, "[offset100]")
	for _, cand := range Walk(b) {
		if cand.Path == targetPath || cand.Path == cleanTarget {
			return cand.Val, true
		}
	}
	return 0, false
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
