#define AST_MODULE_SELF_SYM _chan_discord_self

#include "asterisk.h"
#include "asterisk/astobj2.h"
#include "asterisk/channel.h"
#include "asterisk/frame.h"
#include "asterisk/format_cache.h"
#include "asterisk/logger.h"
#include "asterisk/module.h"
#include "asterisk/rtp_engine.h"
#include "asterisk/stasis_channels.h"
