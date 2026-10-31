-- PL/CSQL part of the DML round (e2e/ops/README.md): sources 11.4 and later have basedb's PL/CSQL function and procedure.
UPDATE app_owner.order_item SET unit_price = app_owner.f_line_total(1, unit_price) + 0.01 WHERE MOD(order_id, 101) = 0;
CALL app_owner.p_close_order(5);
