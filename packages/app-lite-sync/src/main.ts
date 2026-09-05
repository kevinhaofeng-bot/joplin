import { runServer } from './server';

runServer(process.stdin, process.stdout).catch(() => {
	process.stderr.write('SIDECAR_FATAL\n');
	process.exitCode = 1;
});
