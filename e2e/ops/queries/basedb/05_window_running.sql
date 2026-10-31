-- Running SUM, LAG and LEAD over one customer's orders.
SELECT id, ordered_on, total,
       SUM(total) OVER (ORDER BY id) AS running,
       LAG(total) OVER (ORDER BY id) AS prev_total,
       LEAD(ordered_on) OVER (ORDER BY id) AS next_on
FROM app_owner.orders
WHERE customer_id = 8
ORDER BY id;
