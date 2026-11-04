SET ROLE gateway_witness_server_program;
COPY (SELECT 1) TO PROGRAM '/bin/cat > /dev/null';
