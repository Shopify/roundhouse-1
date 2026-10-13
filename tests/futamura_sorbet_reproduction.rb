# The part of sorbet-runtime tests/futamura_sorbet_runtime.rb needs when the gem is not installed:
# the wrappers `sig` leaves behind and the signature API the Futamura guards read, in the shape of
# sorbet-runtime 0.6 (lib/types/private/methods/{_methods,call_validation,signature}.rb). Not a
# substitute for the library: no first-call wrapper (the wrapper is installed at once, as after the
# first call), no sig validation beyond one `is_a?` per type. Like the library's, the wrappers are
# defined in files of one directory, apart from the code the sigs are written in.
require_relative "futamura_sorbet_reproduction/methods"
