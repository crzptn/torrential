#include "torrential-engine/cpp/shim.h"
#include <libtorrent/version.hpp>
#include <libtorrent/session.hpp>

namespace torrential {
rust::String libtorrent_version() {
  lt::session_params p;
  lt::session s(p);
  return rust::String(lt::version());
}
}
