#include "asterisk_wrapper.h"

void* rust_ao2_alloc(size_t data_size, ao2_destructor_fn destructor, unsigned int options) {
  return ao2_alloc_options(data_size, destructor, options);
}

void rust_ao2_ref(void* obj, int delta) {
  ao2_ref(obj, delta);
}

struct ast_frame* rust_ast_frdup(struct ast_frame* frame) {
  return ast_frdup(frame);
}

int rust_ast_rtp_engine_register(struct ast_rtp_engine *engine) {
  return ast_rtp_engine_register(engine);
}
