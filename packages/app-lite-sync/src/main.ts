import { runServer } from './server';

runServer(process.stdin, process.stdout).then(exitCode => {
	if (exitCode !== 0) process.exitCode = exitCode;
}).catch(() => {
	process.stderr.write('SIDECAR_FATAL\n');
	process.exitCode = 1;
});
