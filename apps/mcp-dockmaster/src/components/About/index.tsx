import './styles.css';

const About = () => {
  return (
    <div className="about-container">
      <h1>About MCP Dockmaster</h1>
      
      <section className="about-section">
        <h2>Overview</h2>
        <p>
          MCP Dockmaster is a powerful application management platform designed to simplify the deployment, 
          management, and monitoring of AI applications and services. Our platform provides a seamless 
          experience for developers and users alike.
        </p>
      </section>
      
      <section className="about-section">
        <h2>Mission</h2>
        <p>
          Our mission is to democratize access to advanced AI tools and applications by providing 
          an intuitive interface for managing complex software ecosystems. We believe in making 
          technology accessible to everyone, regardless of their technical expertise.
        </p>
      </section>
      
      <section className="about-section">
        <h2>Features</h2>
        <ul>
          <li>Easy application installation and management</li>
          <li>Centralized control panel for all your AI applications</li>
          <li>Access to a curated store of AI applications</li>
          <li>Simplified configuration and deployment</li>
          <li>Real-time monitoring and logging</li>
        </ul>
      </section>
      
      <section className="about-section">
        <h2>Contact</h2>
        <p>
          For more information about MCP Dockmaster, please visit our website or contact our support team.
        </p>
      </section>
    </div>
  );
};

export default About;
