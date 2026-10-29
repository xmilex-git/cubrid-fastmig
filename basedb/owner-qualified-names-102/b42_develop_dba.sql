-- develop, as dba: SP works at all in this mode; isolate trg1_upd from trg1_ai
SELECT * FROM u1.v1_sp ORDER BY 1;
ALTER TRIGGER u1.trg1_ai STATUS INACTIVE;
