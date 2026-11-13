// eliminate_dead_logic fixture: what reaches an output port, a non-CORE master or a dont-touch
// object is kept; everything else (a dead chain, a flop nobody reads, a loop) goes.
module top (a, clk, y);
  input a, clk;
  output y;
  wire n1, n2, n3, n4, n5, l1, l2;
  BUF_X1 live1 (.A(a), .Z(n1));
  BUF_X1 live2 (.A(n1), .Z(y));
  BUF_X1 dead1 (.A(a), .Z(n2));
  BUF_X1 dead2 (.A(n2), .Z(n3));
  DFF_X1 ff_dead (.CLK(clk), .D(n3), .Q(n4));
  BUF_X1 feeds_ram (.A(a), .Z(n5));
  RAM_X1 ram (.A(n5), .Z());
  BUF_X1 loop1 (.A(l2), .Z(l1));
  BUF_X1 loop2 (.A(l1), .Z(l2));
endmodule
