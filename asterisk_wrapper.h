#define AST_MODULE_SELF_SYM _chan_discord_self

#include "asterisk.h"
#include "asterisk/astobj2.h"
#include "asterisk/channel.h"
#include "asterisk/frame.h"
#include "asterisk/format_cache.h"
#include "asterisk/logger.h"
#include "asterisk/module.h"
#include "asterisk/rtp_engine.h"

void* rust_ao2_alloc(size_t data_size, ao2_destructor_fn destructor, unsigned int options);
void rust_ao2_ref(void* obj, int delta);

struct ast_frame* rust_ast_frdup(struct ast_frame* frame);

int rust_ast_rtp_engine_register(struct ast_rtp_engine *engine);
