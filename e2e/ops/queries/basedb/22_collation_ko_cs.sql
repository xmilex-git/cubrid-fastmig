-- utf8_ko_cs table (dba): sort order and range predicates on the indexed Korean text.
SELECT id, txt FROM dba.ko_text ORDER BY txt, id LIMIT 30;
SELECT COUNT(*), MIN(txt), MAX(txt) FROM dba.ko_text WHERE txt > '가나다19' AND txt < '가나다5';
