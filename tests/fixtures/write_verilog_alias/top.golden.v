module top (in_a,
    out_y,
    io_b,
    same);
 input in_a;
 output out_y;
 inout io_b;
 input same;

 wire n_in;
 wire n_out;
 wire n_io;

 BUF_X1 u1 (.A(n_in),
    .Z(n_out));
 BUF_X1 u2 (.A(n_io));
 BUF_X1 u3 (.A(same));
 assign n_in = in_a;
 assign out_y = n_out;
 assign io_b = n_io;
endmodule
