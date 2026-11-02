// The functions of the Rust library (crates/dodavo-ffi). They take and give
// JSON; what they return is freed with dodavo_free.
#ifndef DODAVO_H
#define DODAVO_H

char *dodavo_start(const char *options, void (*wake)(void *), void *context);
char *dodavo_poll(void);
char *dodavo_call(const char *call);
void dodavo_free(char *text);

#endif
