// SPDX-License-Identifier: GPL-2.0-only
/*
 * Omit OS LinuxKPI: message formatting.
 *
 * Linux drivers log with printf-style variadic functions. Variadic
 * functions cannot be defined in stable Rust, so the formatting is done
 * here and the finished line is handed to the kernel.
 *
 * Supported conversions: %d %i %u %x %X %s %c %p %pe %%, with the l, ll, z
 * and h length modifiers and zero/space padding to a width. That covers the
 * drivers ported so far; an unsupported conversion is printed literally.
 */

#include <linux/kernel.h>

typedef __builtin_va_list va_list;
#define va_start(ap, last) __builtin_va_start(ap, last)
#define va_arg(ap, type) __builtin_va_arg(ap, type)
#define va_end(ap) __builtin_va_end(ap)

/* Implemented in Rust (kernel/src/linuxkpi). `message` is NUL-terminated. */
void omit_kpi_log(int level, const char *message);

struct buffer {
	char *data;
	size_t size;
	size_t used;
};

static void put(struct buffer *out, char c)
{
	/* Always leave room for the terminator; excess output is dropped. */
	if (out->used + 1 < out->size)
		out->data[out->used++] = c;
}

static void put_string(struct buffer *out, const char *s)
{
	if (!s)
		s = "(null)";
	while (*s)
		put(out, *s++);
}

static void put_number(struct buffer *out, unsigned long long value, unsigned int base,
		       bool negative, bool upper, int width, char pad)
{
	const char *digits = upper ? "0123456789ABCDEF" : "0123456789abcdef";
	char scratch[24];
	int length = 0;

	do {
		scratch[length++] = digits[value % base];
		value /= base;
	} while (value);

	if (negative && pad == '0') {
		put(out, '-');
		width--;
	}
	for (int fill = width - length - (negative && pad == ' '); fill > 0; fill--)
		put(out, pad);
	if (negative && pad == ' ')
		put(out, '-');
	while (length)
		put(out, scratch[--length]);
}

static void format(struct buffer *out, const char *fmt, va_list args)
{
	for (; *fmt; fmt++) {
		if (*fmt != '%') {
			put(out, *fmt);
			continue;
		}

		const char *start = fmt++;
		char pad = ' ';
		int width = 0;
		int longs = 0;

		if (*fmt == '0') {
			pad = '0';
			fmt++;
		}
		while (*fmt >= '0' && *fmt <= '9')
			width = width * 10 + (*fmt++ - '0');
		for (;; fmt++) {
			if (*fmt == 'l')
				longs++;
			else if (*fmt == 'z')
				longs = 1;
			else if (*fmt != 'h')
				break;
		}

		switch (*fmt) {
		case 'd':
		case 'i': {
			long long value = longs >= 2 ? va_arg(args, long long)
					: longs == 1 ? va_arg(args, long)
					: va_arg(args, int);
			unsigned long long magnitude =
				value < 0 ? 0ULL - (unsigned long long)value : (unsigned long long)value;
			put_number(out, magnitude, 10, value < 0, false, width, pad);
			break;
		}
		case 'u':
		case 'x':
		case 'X': {
			unsigned long long value = longs >= 2 ? va_arg(args, unsigned long long)
						 : longs == 1 ? va_arg(args, unsigned long)
						 : va_arg(args, unsigned int);
			put_number(out, value, *fmt == 'u' ? 10 : 16, false, *fmt == 'X', width, pad);
			break;
		}
		case 'p':
			if (fmt[1] == 'e') {
				/* %pe: an error pointer, shown as its number. */
				fmt++;
				long error = (long)va_arg(args, void *);
				put_number(out, 0ULL - (unsigned long long)error, 10,
					   true, false, 0, ' ');
				break;
			}
			put_string(out, "0x");
			put_number(out, (uintptr_t)va_arg(args, void *), 16, false, false, 16, '0');
			break;
		case 's':
			put_string(out, va_arg(args, const char *));
			break;
		case 'c':
			put(out, (char)va_arg(args, int));
			break;
		case '%':
			put(out, '%');
			break;
		default:
			/* Unknown conversion: show it as written. */
			while (start <= fmt && *start)
				put(out, *start++);
			if (!*fmt)
				fmt--;
			break;
		}
	}
}

void omit_kpi_printf(int level, const char *prefix, const char *fmt, ...)
{
	char line[256];
	struct buffer out = { .data = line, .size = sizeof(line), .used = 0 };
	va_list args;

	if (prefix) {
		put_string(&out, prefix);
		put_string(&out, ": ");
	}
	va_start(args, fmt);
	format(&out, fmt, args);
	va_end(args);

	/* The kernel log adds its own line ending. */
	while (out.used && out.data[out.used - 1] == '\n')
		out.used--;
	out.data[out.used] = '\0';
	omit_kpi_log(level, line);
}

int sysfs_emit(char *buf, const char *fmt, ...)
{
	struct buffer out = { .data = buf, .size = 4096, .used = 0 };
	va_list args;

	va_start(args, fmt);
	format(&out, fmt, args);
	va_end(args);
	out.data[out.used] = '\0';
	return (int)out.used;
}
