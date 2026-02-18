# risc-v-sim-web

Web version of [risc-v-sim](https://github.com/nup-csai/risc-v-sim)

## How to run with Docker Compose

### Prerequisites
- Docker and Docker Compose installed
- MongoDB running on your local machine (the container connects to it via `host.docker.internal`)
- GitHub OAuth application credentials (create at https://github.com/settings/developers)

### Setup

1. Clone the repository:
```bash
git clone --recursive https://github.com/robocy-lab/risc-v-sim-web
cd risc-v-sim-web
```

2. Create environment file:
```bash
cp .env.example .env
```

3. Edit `.env` and fill in the required variables:
   - `GITHUB_CLIENT_ID` - Your GitHub OAuth app client ID
   - `GITHUB_CLIENT_SECRET` - Your GitHub OAuth app client secret
   - `JWT_SECRET` - Generate a strong random string (e.g., `openssl rand -base64 32`)

4. Make sure MongoDB is running on your machine (port 27017)

5. Start the application:
```bash
docker build -t meow .
docker run meow
```

The application will be available at http://localhost:3000

## How to use
http://localhost:3000/health should return `Ok`.

http://localhost:3000/api/submit with POST request and `ticks=<ticks>` (text/plain) and `file=<program.s>` (application/octet-stream) should return json if all is ok
