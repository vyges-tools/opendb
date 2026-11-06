# Reference golden (run from this directory): DEF=top.def writes top.golden.v; DEF=shared.def
# prints the writer's error.
read_lef ../insert_buffer/cells.lef
read_def $::env(DEF)
if {[catch {write_verilog $::env(OUT)} m]} { puts "write_verilog: $m" }
