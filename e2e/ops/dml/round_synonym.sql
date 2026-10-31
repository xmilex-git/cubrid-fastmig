-- Synonym part of the DML round (e2e/ops/README.md): sources 11.2 and later have the synonyms of basedb.
UPDATE app_owner.syn_cust SET score = score + 1 WHERE id <= 100;
UPDATE dba.syn_orders SET memo = 'syn ' || id WHERE MOD(id, 97) = 0;
