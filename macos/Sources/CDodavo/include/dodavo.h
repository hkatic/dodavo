// The functions of the Rust library (crates/dodavo-ffi). They take and give
// JSON; what they return is freed with dodavo_free.
#ifndef DODAVO_H
#define DODAVO_H

#include <stddef.h>

char *dodavo_start(const char *options, void (*wake)(void *), void *context);
char *dodavo_poll(void);
char *dodavo_call(const char *call);
void dodavo_free(char *text);
// The built-in sound of an event, by its name in the settings: a WAV file of
// *len bytes that stays in memory, and is not freed. Null for an unknown event.
const unsigned char *dodavo_sound(const char *event, size_t *len);

#endif
